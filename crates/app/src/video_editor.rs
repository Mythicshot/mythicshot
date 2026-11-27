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
use mythicshot_recording::ffmpeg::Found;
use mythicshot_recording::{FrameReader, Recording};

use crate::theme::{self, Strong as _};
use crate::editor_ui::{Editor, Place, applied, in_row, row_label, switch};
use crate::icons::{self, Icon};
use crate::project::{Loaded, OpenGuard, ProjectKey, Restored, Saved, Snapshot, Store, Totals};
use crate::publish::{Published, Target, access::Access, share};
use crate::profiles::{self, Owner};
use crate::profiles_ui::{OwnerChoice, OwnerPrompt};
use crate::publish_ui::{self, Before, EditTarget, Kept, Mode, Picker, PickerOutcome, PublishDone, ResultAction, ResultDialog, Run};
use crate::recorder_ui::{EncodeJob, ExportSpec};
use crate::site::{self, SiteSummary};
use crate::site_ui::{SiteDialog, SiteOutcome};
use crate::settings_ui::{click_style_label, key_style_label};
use crate::tour::{self, StepPlan, TourSummary};
use crate::video::{self, Edit};

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
    /// An upload (then a publish, when asked), or a publish alone, with the tour's title and
    /// what the tour was before, for the result screen, and whether it is an update of a
    /// chapter over the same link ("Update tour", and "Try again" after one).
    Publish(EncodeJob<PublishDone>, Arc<Run>, String, Before, bool),
}

/// What a finished export made.
enum Exported {
    File(PathBuf, u64),
    Tour(TourSummary, TourOutput),
    Site(SiteSummary, String),
    /// With whether it was an update.
    Published(PublishDone, String, Before, bool),
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
            ExportJob::Publish(j, ..) => j.progress(),
        }
    }

    /// The outcome once the export has finished.
    fn take_result(&self) -> Option<Result<Exported, String>> {
        match self {
            ExportJob::File(j) => j.take_result().map(|r| r.map(|(path, size)| Exported::File(path, size))),
            ExportJob::Tour(j, out, _) => j.take_result().map(|r| r.map(|t| Exported::Tour(t, *out))),
            ExportJob::Site(j, _, title) => j.take_result().map(|r| r.map(|s| Exported::Site(s, title.clone()))),
            ExportJob::Publish(j, _, title, before, update) => j.take_result().map(|r| r.map(|done| Exported::Published(done, title.clone(), before.clone(), *update))),
        }
    }

    /// For a publish: whether cancel was asked for (None for any other export).
    fn cancelling(&self) -> Option<bool> {
        match self {
            ExportJob::Publish(_, run, ..) => Some(run.is_cancelled()),
            _ => None,
        }
    }

    /// For a publish past its upload: the images the server has copied so far, when it said (None before that step).
    fn publishing(&self) -> Option<Option<crate::publish::Images>> {
        match self {
            ExportJob::Publish(_, run, ..) => run.publishing(),
            _ => None,
        }
    }

    /// For a publish: who can view the tour, as far as it went.
    fn viewing(&self) -> publish_ui::Viewing {
        match self {
            ExportJob::Publish(_, run, ..) => run.viewing(),
            _ => publish_ui::Viewing::default(),
        }
    }

    /// Whether this is an update of a chapter over the same link, or the publish of one.
    fn is_update(&self) -> bool {
        matches!(self, ExportJob::Publish(_, _, _, _, true))
    }

    /// For an update of a chapter, or the publish of one: the title of the tour it changes.
    fn update_title(&self) -> Option<&str> {
        match self {
            ExportJob::Publish(_, _, title, _, true) => Some(title),
            _ => None,
        }
    }

    /// The editor is going away: a publish is cancelled, since nobody would see its result.
    /// It stops calling the server; any other export runs on to its end. So does an update:
    /// it changes a tour that is already on the site, and stopping it half-way without a word
    /// would leave that tour, its public page and the saved recording at three different
    /// points. It runs to its end in the background, the saved recording stays registered as
    /// open until then, and how it ended is said in the app's notification ([`VideoState`]'s drop).
    fn abandon(&self) {
        if let ExportJob::Publish(_, run, _, _, false) = self {
            run.cancel();
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
/// whether a tour export was asked to stop, `publishing` a publish's step after its upload,
/// `updating` whether the publish is an update of a chapter.
fn progress_message(is_tour: bool, cancelling: Option<bool>, tour_cancelling: bool, publishing: Option<Option<crate::publish::Images>>, updating: bool, progress: f32) -> String {
    match cancelling {
        Some(true) => "Cancelling…".to_string(),
        Some(false) if publishing.is_some() => publish_ui::publishing_message(publishing.flatten()),
        None if tour_cancelling => "Cancelling…".to_string(),
        Some(false) if updating => format!("Updating tour… {:.0}%", progress * 100.0),
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

/// Where an upload keeps its recording.
pub(crate) enum Keeping {
    /// The setting is off.
    Off,
    /// The setting is on, but this computer has no folder for saved recordings.
    NoFolder,
    In(Store),
}

/// Where an upload keeps its recording: nowhere with the setting off; else in the store at
/// `root`, the folder [`Store::default_root`] found.
pub(crate) fn keeping(on: bool, root: Option<PathBuf>) -> Keeping {
    match (on, root) {
        (false, _) => Keeping::Off,
        (true, None) => Keeping::NoFolder,
        (true, Some(root)) => Keeping::In(Store::at(root)),
    }
}

/// Where an upload started now keeps its recording: as the setting says, in this user's folder
/// for saved recordings. Nothing on the disk is looked at.
pub(crate) fn keeping_now() -> Keeping {
    keeping(publish_ui::keeps_recordings(), Store::default_root())
}

/// The export format an editor shows for the one it was asked for: MP4 and WebM need ffmpeg,
/// and give way to GIF only once ffmpeg was looked for and there is none. While the one search
/// of this run is still under way, the format that was asked for stays.
fn usable_format(format: Format, found: Found) -> Format {
    if format.needs_ffmpeg() && found == Found::None { Format::Gif } else { format }
}

/// How a project is saved: the store, the key, the state, and the check that says whether to go on.
type SaveFn<'a> = dyn Fn(&Store, &ProjectKey, &Snapshot<'_>, &dyn Fn() -> bool) -> Result<Saved, String> + 'a;

/// The save a publish uses: the store's own, which stops a copy of the frames when told to.
fn store_save(store: &Store, key: &ProjectKey, snap: &Snapshot<'_>, keep_going: &dyn Fn() -> bool) -> Result<Saved, String> {
    store.save_while(key, snap, keep_going)
}

/// The limit a publish measures the saved recordings against once its own is saved: how many
/// GB they may take (0 for no limit), and how what they take is read. The reading lists every
/// saved project, so it is for the job's thread only.
struct Limit<'a> {
    warn_gb: u32,
    totals: &'a dyn Fn(&Store) -> Totals,
}

/// Whether the save that just made `saved` took the recordings in `store` past the limit: the
/// new total and the limit when it did. The store is listed once, after the save; what it held
/// before is that total less what this save wrote, which is true for a project that was not
/// there before. Without a limit nothing is read. A reading that breaks is no notice, and
/// never an error of the save or of the upload.
fn over_after(store: &Store, saved: &Saved, limit: &Limit<'_>) -> Option<(u64, u32)> {
    if limit.warn_gb == 0 {
        return None;
    }
    let after = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (limit.totals)(store))).ok()?;
    publish_ui::went_over(after.bytes, saved.info.bytes, limit.warn_gb)
}

/// The editor's state as it was when a publish started: what is uploaded, and what is kept.
pub(crate) struct PublishState {
    /// The profile the upload goes with: the kept recording is linked to its tour in it.
    pub(crate) profile: String,
    pub(crate) rec: Arc<Recording>,
    pub(crate) items: Vec<mythicshot_editor::Item>,
    pub(crate) events: Vec<InputEvent>,
    pub(crate) edit: Edit,
}

/// Keep the recording of an upload the site has just committed, as the project of exactly the
/// address the upload went to, the tour and the chapter. Whatever goes wrong here, a panic
/// included, is a reason in the answer and never an error of the upload; so is a cancel that
/// stops a copy of the frames (`keep_going` says no).
///
/// `limit`: for a save that makes a new project, what to measure the saved recordings against
/// afterwards, so the answer says when this save took them past it. `None` for an update, which
/// saves over a project that was there already and never says so.
#[allow(clippy::too_many_arguments)]
fn keep_recording(keeping: &Keeping, save: &SaveFn<'_>, limit: Option<&Limit<'_>>, keep_going: &dyn Fn() -> bool, server: &str, up: &Published, state: &PublishState, title: &str, tour_title: &str) -> Kept {
    let store = match keeping {
        Keeping::Off => return Kept::Off,
        Keeping::NoFolder => return Kept::Failed(publish_ui::NO_FOLDER_FOR_RECORDINGS.to_string()),
        Keeping::In(store) => store,
    };
    let saved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // The address was good enough to upload to, so this never fails.
        let server = crate::publish::check_server(server).map_err(|_| publish_ui::KEEPING_BROKE_OFF.to_string())?;
        let key = ProjectKey { server, tour_id: up.tour_id.clone(), chapter_id: up.chapter_id.clone() };
        let snap = Snapshot { profile: &state.profile, rec: &state.rec, title, tour_title, steps: u32::try_from(up.steps).unwrap_or(u32::MAX), edit: &state.edit, events: &state.events, items: &state.items };
        save(store, &key, &snap, keep_going)
    }));
    match saved {
        Ok(Ok(saved)) => {
            // There is a recording to store as video now.
            crate::project::nudge();
            // Read here, on the job's thread and after the save: never on the UI thread.
            let over = limit.and_then(|limit| over_after(store, &saved, limit));
            Kept::Saved { left_out: saved.left_out, over }
        }
        Ok(Err(why)) => Kept::Failed(why),
        Err(_) => Kept::Failed(publish_ui::KEEPING_BROKE_OFF.to_string()),
    }
}

/// The work of "Publish tour", on the job's own thread and with the state the editor had when
/// it started: write the bundle into a fresh temporary folder, upload and commit it, keep the
/// recording (as `keeping` says), then publish when `publish_now`. The folder is removed
/// afterwards whether it worked or not. `token` is asked for only once the bundle is written.
/// An error means nothing was uploaded, and then nothing is kept either; a recording that
/// cannot be kept never makes it an error. A recording that was kept is measured against the
/// limit of the settings, here on the job's thread.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn publish_job(
    state: &PublishState,
    title: &str,
    tour_title: &str,
    server: &str,
    token: impl FnOnce() -> Result<String, String>,
    target: &Target,
    publish_now: bool,
    keeping: &Keeping,
    worker: &Arc<Run>,
    progress: &dyn Fn(f32),
) -> Result<PublishDone, String> {
    let limit = Limit { warn_gb: publish_ui::warn_gb(), totals: &Store::totals };
    publish_job_with(state, title, tour_title, server, token, target, publish_now, keeping, worker, progress, &store_save, &limit)
}

/// [`publish_job`] with the save and the limit handed in, so a test can make the save fail,
/// panic or wait, and can say what the saved recordings take.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn publish_job_with(
    state: &PublishState,
    title: &str,
    tour_title: &str,
    server: &str,
    token: impl FnOnce() -> Result<String, String>,
    target: &Target,
    publish_now: bool,
    keeping: &Keeping,
    worker: &Arc<Run>,
    progress: &dyn Fn(f32),
    save: &SaveFn<'_>,
    limit: &Limit<'_>,
) -> Result<PublishDone, String> {
    publish_job_full(state, title, tour_title, server, token, target, publish_now, &Access::Public, keeping, worker, progress, save, limit, &|_, _| false)
}

/// [`publish_job`] for "Publish tour", with who can view the tour once it is published. What
/// the site said about that is left in `worker` ([`Run::viewing`]) for the result screen; the
/// addresses are not.
#[allow(clippy::too_many_arguments)]
fn publish_job_as(
    state: &PublishState,
    title: &str,
    tour_title: &str,
    server: &str,
    token: impl FnOnce() -> Result<String, String>,
    target: &Target,
    publish_now: bool,
    access: &Access,
    keeping: &Keeping,
    worker: &Arc<Run>,
    progress: &dyn Fn(f32),
) -> Result<PublishDone, String> {
    let limit = Limit { warn_gb: publish_ui::warn_gb(), totals: &Store::totals };
    let delete = |token: &str, tour_id: &str| crate::tours_ui::delete_created_tour(server, token, tour_id, tour_title);
    publish_job_full(state, title, tour_title, server, token, target, publish_now, access, keeping, worker, progress, &store_save, &limit, &delete)
}

/// An upload error during a restricted publish whose commit answer was lost or not understood:
/// a draft may be on the site with an id nobody knows, so it cannot be remembered or deleted.
/// Only then the error says what publishing such a draft from the Tours page would do.
fn lost_commit_note(error: String, restricted: bool) -> String {
    if restricted && error.starts_with("Could not finish the upload") {
        format!("{error} {}", crate::share_ui::who::DRAFT_MAY_APPEAR)
    } else {
        error
    }
}

/// What a restricted publish that did not finish leaves on the site, and what is done about it:
/// a tour this publish created (`Target::New`) whose first publish call the site did not
/// accept is deleted again through `delete` (true when nothing is left); a tour the site took
/// the restriction for keeps it; any other is remembered so that the Tours page asks before
/// publishing it for anyone. None when there is nothing to say: not restricted, or published.
pub(crate) fn settle_restricted(restricted: bool, target: &Target, finished: &crate::publish::Finished, first: crate::publish::PublishedAs, delete: &dyn Fn(&str) -> bool) -> Option<publish_ui::Left> {
    use crate::publish::Outcome;
    use publish_ui::Left;
    if !restricted {
        return None;
    }
    let id = finished.tour_id.as_str();
    let Outcome::NotPublic { error, .. } = &finished.outcome else {
        // Published as asked (or nothing was published): nothing to ask about.
        publish_ui::settle_restriction(id);
        return None;
    };
    // Public now, and taking it down failed: said as it is; the Tours page offers Unpublish.
    if error == crate::share_ui::who::TOUR_NOT_APPLIED_PUBLIC {
        return Some(Left::Online);
    }
    // The site took the call that carried who can view: the draft keeps it, whoever publishes it.
    if first.accepted {
        publish_ui::settle_restriction(id);
        return Some(Left::Restricted);
    }
    // A draft with nothing restricted on it. One this publish made is taken away again.
    if matches!(target, Target::New) {
        if delete(id) {
            publish_ui::settle_restriction(id);
            return Some(Left::Deleted);
        }
        publish_ui::note_unfinished_restriction(id);
        return Some(Left::StillThere);
    }
    publish_ui::note_unfinished_restriction(id);
    Some(Left::Draft)
}

#[allow(clippy::too_many_arguments)]
fn publish_job_full(
    state: &PublishState,
    title: &str,
    tour_title: &str,
    server: &str,
    token: impl FnOnce() -> Result<String, String>,
    target: &Target,
    publish_now: bool,
    access: &Access,
    keeping: &Keeping,
    worker: &Arc<Run>,
    progress: &dyn Fn(f32),
    save: &SaveFn<'_>,
    limit: &Limit<'_>,
    delete: &dyn Fn(&str, &str) -> bool,
) -> Result<PublishDone, String> {
    let restricted = publish_now && !access.is_public();
    worker.set_viewing(publish_ui::Viewing { restricted, granted: None, left: None });
    with_bundle(state, title, worker, progress, |dir| {
        let token = token()?;
        let mut kept = Kept::Off;
        let (finished, first) = crate::publish::upload_and_publish_as(
            server,
            &token,
            dir,
            target,
            publish_now,
            access,
            |f| {
                progress(PUBLISH_WRITE_SHARE + f * (1.0 - PUBLISH_WRITE_SHARE));
                !worker.is_cancelled()
            },
            // The commit has answered: the project is saved now, before the publish step, so
            // that it matches the site's draft whatever becomes of the publish. A cancel stops
            // a copy of the frames, so it never holds the publish step up.
            |up| {
                // From here until it is settled, a plain publish of this draft would make it
                // public: remembered at once, so even an editor closed half-way leaves it known.
                if restricted {
                    publish_ui::note_unfinished_restriction(&up.tour_id);
                }
                kept = keep_recording(keeping, save, Some(limit), &|| !worker.is_cancelled(), server, up, state, title, tour_title)
            },
            |images| {
                worker.set_publishing(images);
                !worker.is_cancelled()
            },
        )
        .map_err(|e| lost_commit_note(e, restricted))?;
        // A tour that went public as asked is no longer one to ask about, however it was published.
        if matches!(finished.outcome, crate::publish::Outcome::Public { .. }) {
            publish_ui::settle_restriction(&finished.tour_id);
        }
        let left = settle_restricted(restricted, target, &finished, first, &|tour_id| delete(&token, tour_id));
        worker.set_viewing(publish_ui::Viewing { restricted, granted: first.granted, left });
        Ok(PublishDone { finished, kept })
    })
}

/// Write the tour's bundle into a fresh temporary folder, as the publish `worker` stands for,
/// and hand the folder to `then`; the folder is removed afterwards whether it worked or not.
/// The first [`PUBLISH_WRITE_SHARE`] of `progress` is the writing.
fn with_bundle<T>(state: &PublishState, title: &str, worker: &Arc<Run>, progress: &dyn Fn(f32), then: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let tmp = tempfile::Builder::new().prefix(publish_ui::TEMP_PREFIX).tempdir().map_err(|e| format!("Could not create a temporary folder: {e}"))?;
    worker.set_dir(Some(tmp.path().to_path_buf()));
    let result = (|| {
        let dir = tmp.path().join("tour");
        // The editor's reader stays on the UI thread; this one belongs to the publish.
        let mut reader = FrameReader::new(state.rec.clone())?;
        tour::write_bundle(&state.rec, &state.items, &state.events, &state.edit, title, &dir, |i| reader.get(i), |f| {
            progress(f * PUBLISH_WRITE_SHARE);
            !worker.is_cancelled()
        })?;
        if worker.is_cancelled() {
            return Err(crate::publish::CANCELLED.to_string());
        }
        then(&dir)
    })();
    let shown = tmp.path().display().to_string();
    if let Err(e) = tmp.close() {
        eprintln!("Publish tour: could not remove the temporary folder {shown}: {e}");
    }
    result
}

/// The work of "Update tour", on the job's own thread and with the state the editor had when
/// it started: write the bundle, replace the chapter `target` names on the site (only the
/// pictures the site lacks are sent), save the project over the one the editor was opened
/// from, then publish when the site says that is safe. `keeping` is the store that project
/// lives in. An error means the project on this computer was not touched; and the site's
/// chapter is as it was, unless the error itself says to check the tour list.
///
/// The save runs once, when the commit has answered and before anything is published, so the
/// saved project matches the site's draft whatever becomes of the publish. It is not stopped
/// by a cancel: the site already has the changes. A save that fails never fails the update;
/// the result says that the saved recording no longer matches.
#[allow(clippy::too_many_arguments)]
fn update_job(state: &PublishState, title: &str, target: &EditTarget, server: &str, token: impl FnOnce() -> Result<String, String>, keeping: &Keeping, worker: &Arc<Run>, progress: &dyn Fn(f32)) -> Result<PublishDone, String> {
    update_job_with(state, title, target, server, token, keeping, worker, progress, &store_save)
}

/// [`update_job`] with the save handed in, so a test can make it fail or panic.
#[allow(clippy::too_many_arguments)]
fn update_job_with(
    state: &PublishState,
    title: &str,
    target: &EditTarget,
    server: &str,
    token: impl FnOnce() -> Result<String, String>,
    keeping: &Keeping,
    worker: &Arc<Run>,
    progress: &dyn Fn(f32),
    save: &SaveFn<'_>,
) -> Result<PublishDone, String> {
    with_bundle(state, title, worker, progress, |dir| {
        let token = token()?;
        // Until the save has answered, the saved project is not known to match the site.
        let mut kept = Kept::Failed(publish_ui::KEEPING_BROKE_OFF.to_string());
        let finished = crate::publish::update_tour(
            server,
            &token,
            dir,
            &target.tour_id,
            &target.chapter_id,
            |f| {
                progress(PUBLISH_WRITE_SHARE + f * (1.0 - PUBLISH_WRITE_SHARE));
                !worker.is_cancelled()
            },
            |up| kept = keep_recording(keeping, save, None, &|| true, server, up, state, title, &target.tour_title),
            |images| {
                worker.set_publishing(images);
                !worker.is_cancelled()
            },
        )?;
        Ok(PublishDone { finished, kept })
    })
}

/// What a shared clip is prepared from: the recording as the editor has it, and the export
/// menu's choices.
pub(crate) struct ClipSpec {
    pub rec: Arc<Recording>,
    pub items: Vec<mythicshot_editor::Item>,
    pub events: Vec<InputEvent>,
    pub edit: Edit,
    pub media: share::MediaType,
    /// GIF quality, as chosen in the export menu.
    pub quality: u8,
    /// MP4 frame rate, as chosen in the export menu.
    pub fps: u32,
}

/// A shared clip is an MP4 when ffmpeg is there, else a GIF.
fn clip_media(ffmpeg: bool) -> share::MediaType {
    if ffmpeg { share::MediaType::Mp4 } else { share::MediaType::Gif }
}

/// How long a clip of these output frames runs, from its first frame to its end; at least 1 ms.
fn clip_duration_ms(frames: &[mythicshot_recording::export::OutFrame], end_ms: u64) -> u64 {
    end_ms.saturating_sub(frames.first().map_or(0, |f| f.t_ms)).max(1)
}

/// Prepare the clip in a new folder in `temp` through the export, check its real size against
/// what any site can take (the configured site's own limit is said by its answer to the
/// create, before any part is sent), then share it with its first frame as the preview. The folder is removed
/// whatever happens. The MP4 has no sound: Mythicshot records none, and the export asks ffmpeg
/// for a video stream only.
pub(crate) fn share_clip_job(clip: &ClipSpec, temp: &Path, server: &str, title: String, access: &crate::publish::access::Access, token: impl FnOnce() -> Result<String, String>, r: &crate::share_ui::Reporter) -> Result<share::Shared, share::ShareError> {
    use share::ShareError;
    let dir = tempfile::Builder::new().prefix(crate::share_ui::TEMP_PREFIX).tempdir_in(temp).map_err(|_| ShareError::Failed("Could not make a temporary folder for the clip.".to_string()))?;
    let dir_path = dir.path().to_path_buf();
    r.keep_dir(Some(dir_path.clone()));
    let result = prepare_and_share(clip, &dir_path, server, title, access, token, r);
    r.keep_dir(None);
    drop(dir);
    crate::share_ui::remove_soon(&dir_path);
    result
}

fn prepare_and_share(clip: &ClipSpec, dir: &Path, server: &str, title: String, access: &crate::publish::access::Access, token: impl FnOnce() -> Result<String, String>, r: &crate::share_ui::Reporter) -> Result<share::Shared, share::ShareError> {
    use mythicshot_recording::export::{self, Job};
    use share::ShareError;
    let format = match clip.media {
        share::MediaType::Mp4 => Format::Mp4,
        _ => Format::Gif,
    };
    let path = dir.join(format!("clip.{}", format.extension()));
    let (frames, end_ms) = video::plan(&clip.rec, &clip.edit);
    let (width, height) = video::output_size(&clip.rec, &clip.edit);
    let duration_ms = clip_duration_ms(&frames, end_ms);
    let first = frames.first().map(|f| f.src);
    // The page plays a clip in a loop, so a GIF loops too.
    let job = Job { frames, end_ms, width, height, format, quality: clip.quality, loop_forever: true, video_fps: clip.fps };
    let (rec, items, events, edit) = (&clip.rec, &clip.items, &clip.events, &clip.edit);
    let exported = export::export(clip.rec.clone(), &job, &path, |src, img| video::decorate(rec, src, img, items, events, edit, true), |f| r.preparing(f));
    if r.cancelled() {
        return Err(ShareError::Cancelled);
    }
    let bytes = exported.map_err(|e| ShareError::Failed(format!("Could not prepare the clip: {e}")))?;
    if let Some(t) = share::too_large(share::Kind::Clip, Some(bytes), true, width, height, Some(duration_ms)) {
        return Err(ShareError::TooLarge(t));
    }
    let data = std::fs::read(&path).map_err(|_| ShareError::Failed("Could not read the prepared clip.".to_string()))?;
    // The preview is the clip's first frame as it plays; a clip without one goes without.
    let preview = first.and_then(|src| {
        let mut reader = FrameReader::new(rec.clone()).ok()?;
        let img = video::decorate(rec, src, reader.get(src).ok()?, items, events, edit, true);
        share::preview_of(&img)
    });
    let token = token().map_err(ShareError::Failed)?;
    let item = share::Item { media: clip.media, data, width, height, duration_ms: Some(duration_ms), title, preview };
    share::share(server, &token, &item, access, |sent, total| r.uploading(sent, total))
}

/// The tour and chapter a publish or a draft upload of a new tour or chapter from this editor
/// put on the site: what its Publish dialog offers to replace from then on, as for an editor
/// opened through Edit. Its key is the one the upload's saved project has.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Remembered {
    pub(crate) key: ProjectKey,
    /// The profile the upload went with: the tour belongs to it.
    pub(crate) profile: String,
    /// The chapter's title as it was uploaded: a replace sends it again, so it is not renamed.
    pub(crate) chapter_title: String,
    pub(crate) tour_title: String,
    /// The upload's recording was saved on this computer: a replace saves over it whatever the
    /// keep setting says now, as an update of an opened project does.
    pub(crate) kept: bool,
}

/// What a publish started from this editor goes to, until it has ended.
#[derive(Debug, Clone, PartialEq)]
struct Pending {
    /// The profile the publish goes with.
    profile: String,
    server: String,
    chapter_title: String,
}

/// What an editor remembers once the publish `pending` ended with `done`: the tour and chapter
/// it uploaded, under the address rule's form of the address, as the saved project is keyed.
/// Nothing when nothing is left on the site to replace: a tour taken away again after a
/// restricted publish did not finish, or an answer without a chapter.
fn remember_upload(pending: &Pending, tour_title: &str, done: &PublishDone, left: Option<publish_ui::Left>) -> Option<Remembered> {
    let f = &done.finished;
    if left == Some(publish_ui::Left::Deleted) || f.tour_id.is_empty() || f.chapter_id.is_empty() {
        return None;
    }
    let server = crate::publish::check_server(&pending.server).unwrap_or_else(|_| pending.server.clone());
    let key = ProjectKey { server, tour_id: f.tour_id.clone(), chapter_id: f.chapter_id.clone() };
    Some(Remembered { profile: pending.profile.clone(), key, chapter_title: pending.chapter_title.clone(), tour_title: tour_title.to_string(), kept: matches!(done.kept, Kept::Saved { .. }) })
}

/// The tour and chapter an editor's Publish dialog replaces, and how.
#[derive(Debug, Clone, PartialEq)]
struct Replaceable {
    key: ProjectKey,
    /// The profile the tour belongs to, as recorded (empty: linked before profiles).
    profile: String,
    target: EditTarget,
    /// The chapter's title to send; None for the editor's own title (an opened project's).
    chapter_title: Option<String>,
    /// Save over the project whatever the keep setting says.
    keep_always: bool,
    logo_missing: bool,
    /// The site said this chapter is no longer there, when it was last replaced from this editor:
    /// Replace is not offered again, and its tour stays picked for a new chapter.
    gone: bool,
}

/// What `v`'s Publish dialog replaces: the chapter this editor uploaded last, else the one it
/// was opened from through Edit; None for a new recording that was never uploaded.
fn replaceable(v: &VideoState) -> Option<Replaceable> {
    if let Some(r) = &v.remembered {
        let target = EditTarget { tour_id: r.key.tour_id.clone(), chapter_id: r.key.chapter_id.clone(), tour_title: r.tour_title.clone() };
        // Its upload went without the logo if it had none: replacing it takes nothing away.
        let gone = v.chapter_gone.as_ref() == Some(&r.key);
        return Some(Replaceable { key: r.key.clone(), profile: r.profile.clone(), target, chapter_title: Some(r.chapter_title.clone()), keep_always: r.kept, logo_missing: false, gone });
    }
    let origin = v.origin.as_ref()?;
    let gone = v.chapter_gone.as_ref() == Some(&origin.key);
    Some(Replaceable { key: origin.key.clone(), profile: origin.profile.clone(), target: edit_target(origin), chapter_title: None, keep_always: true, logo_missing: logo_missing(origin, &v.edit), gone })
}

/// The chapter to stop offering to replace after an update of `r` failed with `error`: only
/// when the site said the chapter is no longer part of the tour. A tour that is gone is seen in
/// the site's list by every new dialog, and any other failure may work the next time.
fn gone_after(error: &str, r: Option<&Replaceable>) -> Option<ProjectKey> {
    crate::publish::is_chapter_gone(error).then(|| r.map(|r| r.key.clone())).flatten()
}

/// What "Update tour" replaces for an editor opened from the project `origin`.
fn edit_target(origin: &Origin) -> EditTarget {
    EditTarget { tour_id: origin.key.tour_id.clone(), chapter_id: origin.key.chapter_id.clone(), tour_title: origin.tour_title.clone() }
}

/// Whether `target` is the tour and chapter the project `origin` was saved for. The title is
/// not part of that: the dialog hands on the tour's title as the site has it now, which is
/// another one after a rename.
#[cfg(test)]
fn is_origin(target: &EditTarget, origin: &Origin) -> bool {
    is_chapter(target, &origin.key)
}

/// Whether `target` is the tour and chapter of `key`, whatever the titles.
fn is_chapter(target: &EditTarget, key: &ProjectKey) -> bool {
    target.tour_id == key.tour_id && target.chapter_id == key.chapter_id
}

/// Which Publish dialog an editor opens.
#[derive(Debug, Clone, PartialEq)]
enum Dialog {
    /// A new recording: a new tour, or a new chapter of one; with the recording's title.
    New(String),
    /// Replace the chapter, add a new chapter or make a new tour; `title` is the chapter's.
    Replace { title: String, target: EditTarget, logo_missing: bool },
    /// A new chapter (of the chapter's tour, picked) or a new tour: the site said the chapter is gone.
    ChapterGone { title: String, target: EditTarget },
    /// The same, for a chapter of another site address than the one configured: no Replace.
    Elsewhere { title: String, target: EditTarget },
    /// The tour belongs to another profile than the active one, or to one that was removed:
    /// the update is refused with the question, before any dialog.
    OtherProfile { owner: Owner, title: String },
}

/// The Publish dialog of `v` for the site at `server`; `title` is the editor's.
fn dialog_for(v: &VideoState, server: &str, title: String) -> Dialog {
    dialog_in(v, server, title, &publish_ui::book())
}

/// [`dialog_for`] with the profiles given: a tour of another profile than the active one is
/// never updated from here, whatever its address.
fn dialog_in(v: &VideoState, server: &str, title: String, book: &profiles::Book) -> Dialog {
    if let Some(r) = replaceable(v)
        && let owner @ (Owner::Other { .. } | Owner::Removed) = profiles::owner_of(book, &r.profile)
    {
        return Dialog::OtherProfile { owner, title };
    }
    match replaceable(v) {
        None => Dialog::New(title),
        Some(r) if r.gone && on_this_site(&r.key, server) => Dialog::ChapterGone { title: r.chapter_title.unwrap_or(title), target: r.target },
        Some(r) if on_this_site(&r.key, server) => Dialog::Replace { title: r.chapter_title.unwrap_or(title), target: r.target, logo_missing: r.logo_missing },
        Some(r) => Dialog::Elsewhere { title: r.chapter_title.unwrap_or(title), target: r.target },
    }
}

/// Whether the dialog should say that updating removes the tour's cover logo: the project was
/// opened without its logo, and the tour's cover has not been given one since.
fn logo_missing(origin: &Origin, edit: &Edit) -> bool {
    origin.logo_dropped && edit.tour_doc.logo_path.is_none()
}

/// Whether the active profile of `book` may update `r`'s tour: it belongs to it, or it is a tour
/// linked before profiles that nobody owns yet (then only at its own address, which the caller
/// checks; the update's save stamps the profile on it). With no profile at all nothing is sent.
fn owned_by(r: &Replaceable, book: &profiles::Book) -> bool {
    matches!(profiles::owner_of(book, &r.profile), Owner::Active | Owner::Unowned | Owner::NoProfile)
}

/// Whether the project `key` belongs to the site at `server`: projects are tied to the exact
/// address the address rule writes, so a tour of another address is never updated from here.
fn on_this_site(key: &ProjectKey, server: &str) -> bool {
    crate::publish::check_server(server).is_ok_and(|s| s == key.server)
}

/// "Try again": the publish step alone, for the upload `retry` names. Nothing is written,
/// uploaded or saved again; what the first result said about the recording is said again.
fn retry_done(retry: &publish_ui::Retry, result: Result<crate::publish::Live, String>) -> PublishDone {
    PublishDone { finished: crate::publish::after_publish(&retry.tour_id, &retry.chapter_id, retry.steps, result), kept: retry.kept.clone() }
}

/// The status-bar message when a publish or an update ended without uploading: "cancelled" in
/// words, else the error as it is.
fn failed_publish_message(error: &str, updating: bool) -> String {
    match (error == crate::publish::CANCELLED, updating) {
        (true, true) => "Update cancelled".to_string(),
        (true, false) => "Publishing cancelled".to_string(),
        (false, _) => error.to_string(),
    }
}

/// What jobs that ran on after their editor was closed have to say to the user. The editor's
/// status bar is gone by then, so the app shows each one in its notification, once.
#[derive(Clone, Default)]
pub(crate) struct Notices(Arc<std::sync::Mutex<Vec<String>>>);

impl Notices {
    fn post(&self, text: String) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    }

    /// What was posted since the last call, oldest first.
    pub(crate) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// The notices of every editor of this app, which the app reads once a frame.
pub(crate) fn background_notices() -> Notices {
    static NOTICES: std::sync::LazyLock<Notices> = std::sync::LazyLock::new(Notices::default);
    NOTICES.clone()
}

/// What to tell the user when a job ends after its editor was closed. Only an update says
/// anything (`update_title` is its tour's title; None for every other job): a publish was
/// cancelled with the editor, and a file export leaves its file. An update that worked says
/// what the status bar would have said; one that did not, the sentence the dialog would have
/// shown, after the tour's title, since no window is left to say which tour it was about.
/// Never the link, and an error never holds the token.
fn outlived_message(result: &Result<Exported, String>, update_title: Option<&str>) -> Option<String> {
    let title = update_title?;
    match result {
        Ok(Exported::Published(done, _, before, true)) => Some(publish_ui::result_status(&done.finished, before, true)),
        Ok(_) => None,
        Err(e) => Some(format!("\"{}\": {}", publish_ui::short_title(title, publish_ui::ROW_TITLE_MAX), failed_publish_message(e, true))),
    }
}

/// The saved project an editor was opened from.
pub struct Origin {
    pub key: ProjectKey,
    /// The profile the tour belongs to, as the project says (empty: saved before profiles).
    pub profile: String,
    /// The tour's title: as saved with the recording, and after an update as the site had it then.
    pub tour_title: String,
    /// The cover logo saved with the recording could not be opened with it.
    logo_dropped: bool,
    /// Keeps the project registered as open, for as long as this editor or a job it started
    /// reads the recording. Shared, so that whichever of them ends last lets go of it.
    open: Arc<OpenGuard>,
}

/// Before the list of what a saved project was opened without. The wording is proposed and waits for the owner's approval.
pub const OPENED_WITHOUT: &str = "This recording was opened without ";

/// The status line when a saved project opens without something it was saved with.
pub fn opened_note(dropped: &[String]) -> Option<String> {
    if dropped.is_empty() { None } else { Some(format!("{OPENED_WITHOUT}{}", publish_ui::listed(dropped))) }
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
    RichText::new(format!("\n{hint}")).color(theme::MUTED).size(12.0).append_to(&mut job, ui.style(), egui::FontSelection::Default, egui::Align::Min);
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
    job: Option<ExportJob>,
    /// The "Publish tour" dialog, while it is open.
    picker: Option<Picker>,
    /// The question an update of another profile's tour is refused with, while it is open.
    owner_prompt: Option<OwnerPrompt>,
    /// The profile the open dialog was made for: a switch while it is open makes it again.
    picker_for: Option<Option<String>>,
    /// The profile and address the last publish or update of this editor went with: "Try again"
    /// publishes there, whatever profile is active by then.
    job_pin: Option<crate::profiles::Pin>,
    /// The "Publish tour" dialog as the user left it when the running publish or update was
    /// started from it: it comes back like that when the site has no room for the tour.
    picker_left: Option<Picker>,
    /// The screen after a publish or a draft upload, until "Done".
    publish_result: Option<ResultDialog>,
    /// The "Add to site" dialog, while it is open.
    site_dialog: Option<SiteDialog>,
    pub exported: bool,
    pub close_after_export: bool,
    drag: Option<TimelineDrag>,
    /// Overlay style last reported to the app, to notice changes.
    last_overlays: OverlayStyle,
    /// Where an update that outlives this editor says how it ended.
    notices: Notices,
    /// A "Publish to" job of this recording, until it has ended: the recording's files stay until then.
    pub(crate) share_waiter: Option<crate::share_ui::JobWaiter>,
    /// The tour and chapter this editor uploaded last: its Publish dialog replaces it from then on.
    pub(crate) remembered: Option<Remembered>,
    /// Where the publish running now goes, until it has ended.
    publishing_to: Option<Pending>,
    /// A chapter the site said it no longer has when this editor tried to replace it.
    chapter_gone: Option<ProjectKey>,
    /// The saved project this editor was opened from; None for a new recording. Declared last:
    /// it lets go of the project only after the reader above has closed the frames file.
    pub origin: Option<Origin>,
}

impl Drop for VideoState {
    /// The recording's temp files go with the editor; an export still running
    /// keeps them until it finishes. A publish is cancelled: nobody would see its result.
    /// An update is not: it runs to its end ([`ExportJob::abandon`] says why), and how it ended
    /// is handed to the app's notification.
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.abandon();
        }
        let rec = self.rec.clone();
        // A recording opened from a saved project is not this editor's to delete, and
        // `delete_files` does nothing for it. The project stays registered as open until both
        // this editor's reader and a job still running have stopped reading it.
        let open = self.origin.as_ref().map(|o| o.open.clone());
        let notices = self.notices.clone();
        // A "Publish to" job was cancelled with its window; it may still be reading frames.
        let share = self.share_waiter.take().filter(|w| !w.is_finished());
        match self.job.take() {
            None if share.is_none() => rec.delete_files(),
            job => {
                std::thread::spawn(move || {
                    if let Some(job) = job {
                        let result = loop {
                            match job.take_result() {
                                Some(result) => break result,
                                None => std::thread::sleep(std::time::Duration::from_millis(200)),
                            }
                        };
                        // An update ran on without its editor: how it ended goes to the log, and to
                        // the app's notification, since a release build shows no log.
                        if let Some(msg) = outlived_message(&result, job.update_title()) {
                            eprintln!("Update tour, after its editor was closed: {msg}");
                            notices.post(msg);
                        }
                        drop(result);
                    }
                    while share.as_ref().is_some_and(|w| !w.is_finished()) {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    rec.delete_files();
                    drop(rec);
                    drop(open);
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
        // ffmpeg may still be looked for when the first editor opens: see `usable_format`.
        let format = usable_format(format, mythicshot_recording::ffmpeg::found());
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
            last_overlays: overlays,
            notices: background_notices(),
            share_waiter: None,
            job: None,
            picker: None,
            owner_prompt: None,
            picker_for: None,
            job_pin: None,
            picker_left: None,
            publish_result: None,
            site_dialog: None,
            exported: false,
            close_after_export: false,
            drag: None,
            remembered: None,
            publishing_to: None,
            chapter_gone: None,
            origin: None,
        });
        editor.init_pulse_history();
        // A recording opens in the Timeline view: its panel as it was left last time.
        editor.panel_open = editor.prefs.timeline_properties_open;
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

    /// Open a saved project in an editor: the recording, with the edit, the mouse events and
    /// the annotations it was saved with (a saved project holds no key presses). The editor
    /// knows where it came from ([`VideoState::origin`]) and keeps the project registered as
    /// open until it closes. Closing it never asks to export, and deletes nothing: the
    /// recording stays saved. What could not be restored is named in the status bar. When the
    /// recording cannot be read after all, the project is let go of again.
    pub fn open_project(loaded: Loaded, keys: std::collections::BTreeMap<crate::hotkeys::EditorAction, String>, dest: Context, overlays: OverlayStyle, prefs: crate::settings::EditorPrefs) -> Result<Editor, String> {
        let Loaded { info, rec, restored, open } = loaded;
        let Restored { edit, events, items, title, dropped } = restored;
        // `new_video` numbers the items from 1 in their saved order, as they were when restored,
        // and the next new item gets the number after the last of them.
        let mut editor = Editor::new_video(rec, events, items, title, keys, dest, overlays, prefs)?;
        if let Some(v) = editor.video.as_mut() {
            v.current = edit.trim.0.min(v.rec.frames.len().saturating_sub(1));
            // The saved overlay style is this recording's, not a new default for the next one.
            v.last_overlays = edit.overlays;
            v.edit = edit;
            // The close question says the recording is deleted, which is untrue here.
            v.exported = true;
            let logo_dropped = dropped.iter().any(|d| d == crate::project::COVER_LOGO);
            v.origin = Some(Origin { key: info.key, profile: info.profile, tour_title: info.tour_title, logo_dropped, open: Arc::new(open) });
        }
        // The moved pulses it was saved with are where undo starts from.
        editor.init_pulse_history();
        // Kept in front of whatever the editor reports next, until the user has had the time to read it.
        if let Some(note) = opened_note(&dropped) {
            editor.notify_held(note);
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
        // An update of another profile's tour: the question first.
        let mut reopen = false;
        if let Some(choice) = v.owner_prompt.as_ref().and_then(|p| p.show(ctx)) {
            let title = v.owner_prompt.take().map(|p| p.title).unwrap_or_default();
            match choice {
                OwnerChoice::Switch(id) => reopen = publish_ui::switch_profile(&id),
                OwnerChoice::NewTour => {
                    v.picker = Some(Picker::open(publish_ui::server(), title));
                    v.picker_for = Some(publish_ui::pin().profile);
                }
                OwnerChoice::Cancel => {}
            }
        }
        // A switch of profile while the dialog is open: it is made again for the new one.
        if v.picker.is_some() && v.job.is_none() && v.picker_for.as_ref().is_some_and(|p| *p != publish_ui::pin().profile) {
            v.picker = None;
            reopen = true;
        }
        if reopen {
            self.open_publish_picker();
        }
        let Some(v) = self.video.as_mut() else { return };
        // The "Publish tour" dialog. "Publish" or "Upload draft" either starts the upload or
        // keeps the dialog open with the reason it cannot.
        match v.picker.as_mut().and_then(|p| p.show(ctx)) {
            None => {}
            Some(PickerOutcome::Cancel) => v.picker = None,
            Some(PickerOutcome::Upload { title, target, mode, tour_title, before, access }) => {
                let has_steps = tour::plan_steps(&v.rec, &v.edit, &v.events).is_ok();
                match publish_ui::publish_blocked(v.job.is_some(), has_steps, &publish_ui::server(), &publish_ui::token_status(), publish_ui::token_site().as_deref()) {
                    Some(blocked) => {
                        if let Some(p) = v.picker.as_mut() {
                            p.refuse(blocked.refusal());
                        }
                    }
                    None => {
                        v.picker_left = v.picker.take();
                        self.start_publish(title, target, mode, tour_title, before, access);
                    }
                }
            }
            Some(PickerOutcome::Update { title, target, before }) => {
                let has_steps = tour::plan_steps(&v.rec, &v.edit, &v.events).is_ok();
                match publish_ui::publish_blocked(v.job.is_some(), has_steps, &publish_ui::server(), &publish_ui::token_status(), publish_ui::token_site().as_deref()) {
                    Some(blocked) => {
                        if let Some(p) = v.picker.as_mut() {
                            p.refuse(blocked.refusal());
                        }
                    }
                    None => {
                        v.picker_left = v.picker.take();
                        self.start_update(title, target, before);
                    }
                }
            }
        }
        // The screen after a publish: "Done" closes it, "Try again" publishes the uploaded draft.
        let Some(v) = self.video.as_mut() else { return };
        match v.publish_result.as_mut().and_then(|d| d.show(ctx)) {
            None => {}
            Some(ResultAction::Done) => v.publish_result = None,
            Some(ResultAction::Open(url)) => {
                // The opener checks the link itself, and its error never repeats the link.
                if let Err(e) = dest::open_url(&url) {
                    eprintln!("Publish tour: could not open the browser: {e}");
                    self.notify("Could not open the browser. Copy the link instead.");
                }
            }
            Some(ResultAction::Retry) => {
                // The screen is only taken away when the publish can start.
                let running = v.job.is_some();
                if let Some(retry) = publish_ui::take_retry(&mut v.publish_result, running) {
                    self.start_publish_only(retry);
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
        let viewing = v.job.as_ref().map(|j| j.viewing()).unwrap_or_default();
        let progress = v.job.as_ref().map(|j| j.progress());
        let is_tour = v.job.as_ref().is_some_and(|j| j.is_tour());
        let is_site = v.job.as_ref().is_some_and(|j| j.is_site());
        let cancelling = v.job.as_ref().and_then(|j| j.cancelling());
        let updating = v.job.as_ref().is_some_and(|j| j.is_update());
        let publishing = v.job.as_ref().and_then(|j| j.publishing());
        let tour_cancelling = v.job.as_ref().and_then(|j| j.tour_cancelling()) == Some(true);
        if let Some(r) = finished {
            v.job = None;
            let left = v.picker_left.take();
            // Only a publish that uploaded is remembered: the Published branch below takes it.
            if r.is_err() {
                v.publishing_to = None;
            }
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
                        Exported::Published(done, title, before, update) => {
                            // Never the link itself in the log: the result screen shows it.
                            // The status line here is proposed and awaits the owner's approval.
                            let msg = if viewing.left == Some(publish_ui::Left::Deleted) {
                                format!("{}. {}", crate::share_ui::who::NOT_PUBLISHED_HEADING, crate::share_ui::who::NOTHING_LEFT)
                            } else {
                                publish_ui::result_status(&done.finished, &before, update)
                            };
                            eprintln!("{msg}");
                            self.notify(msg);
                            if let Some(v) = self.video.as_mut() {
                                let pending = v.publishing_to.take();
                                if update {
                                    // The saved project now holds the title the update was sent under.
                                    match (v.remembered.as_mut(), v.origin.as_mut()) {
                                        (Some(r), _) => r.tour_title = title.clone(),
                                        (None, Some(origin)) => origin.tour_title = title.clone(),
                                        (None, None) => {}
                                    }
                                } else if let Some(pending) = pending {
                                    // From now on Publish offers to replace what was just uploaded.
                                    if let Some(r) = remember_upload(&pending, &title, &done, viewing.left) {
                                        v.remembered = Some(r);
                                    }
                                }
                                v.publish_result = Some(if update { ResultDialog::after_update(title, before, done) } else { ResultDialog::new(title, before, done) }.with_viewing(viewing));
                            }
                        }
                    }
                }
                Err(e) if cancelling.is_some() => {
                    let msg = failed_publish_message(&e, updating);
                    eprintln!("Publish tour: {msg}");
                    self.notify(msg);
                    // An update that failed: the dialog comes back with the reason, and still
                    // offers a new tour. A cancel is the user's own doing and brings nothing back.
                    // A workspace without room brings back the dialog as it was left, for a
                    // new tour or chapter too: its message is more than a status line.
                    if updating && let Some(v) = self.video.as_mut() {
                        // A chapter the site no longer has is not offered as the default again.
                        if let Some(key) = gone_after(&e, replaceable(v).as_ref()) {
                            v.chapter_gone = Some(key);
                        }
                    }
                    self.bring_dialog_back(left, e, updating);
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
            let msg = if is_site { site_progress_message(tour_cancelling, p) } else { progress_message(is_tour, cancelling, tour_cancelling, publishing, updating, p) };
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
        if let Some(parent) = crate::dialogs::pick_folder(rfd::FileDialog::new().set_directory(&self.dest.output_dir), "tour folder") {
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
        if let Some(path) = crate::dialogs::save_file(rfd::FileDialog::new().set_directory(&self.dest.output_dir).set_file_name(&name).add_filter("Web page", &["html"]), "tour file") {
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
        self.video.as_ref().is_some_and(|v| v.picker.is_some() || v.owner_prompt.is_some() || v.publish_result.is_some())
    }

    /// "Publish tour…" can be used now: what makes the export panel's button usable.
    pub(crate) fn can_publish_tour(&self) -> bool {
        let Some(v) = self.video.as_ref() else { return false };
        let tour_ok = crate::tour_view::planned_steps(v).is_ok();
        publish_ui::publish_blocked(self.exporting(), tour_ok, &publish_ui::server(), &publish_ui::token_status(), publish_ui::token_site().as_deref()).is_none()
    }

    pub(crate) fn open_publish_picker(&mut self) {
        let title = self.title().to_string();
        let Some(v) = self.video.as_mut() else { return };
        if v.job.is_some() || v.picker.is_some() || v.owner_prompt.is_some() || v.publish_result.is_some() || v.site_dialog.is_some() {
            return;
        }
        let server = publish_ui::server();
        v.picker = Some(match dialog_for(v, &server, title) {
            Dialog::New(title) => Picker::open(server, title),
            Dialog::Replace { title, target, logo_missing } => Picker::open_for_edit(server, title, target).with_logo_missing(logo_missing),
            Dialog::Elsewhere { title, target } => Picker::open_for_edit_elsewhere(server, title, target),
            Dialog::ChapterGone { title, target } => Picker::open_for_edit_chapter_gone(server, title, target),
            Dialog::OtherProfile { owner, title } => {
                v.owner_prompt = Some(OwnerPrompt { owner, title });
                return;
            }
        });
        v.picker_for = Some(publish_ui::pin().profile);
    }

    /// "Publish to…" in the Export menu: share the clip with a link. It is prepared with the
    /// size, quality, frame rate and trim chosen in the menu, as MP4 when ffmpeg was found and as
    /// a GIF otherwise, and the window says why a GIF is larger. A clip over the site's limits
    /// on its sides or its length is refused before anything is prepared; its size is known
    /// only once it is ([`share_clip_job`]).
    fn open_share_clip(&mut self) {
        let title = self.title().to_string();
        if self.share.is_some() {
            return;
        }
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        let media = clip_media(ffmpeg_available());
        let (width, height) = video::output_size(&v.rec, &v.edit);
        let (frames, end_ms) = video::plan(&v.rec, &v.edit);
        let duration_ms = clip_duration_ms(&frames, end_ms);
        let clip = ClipSpec { rec: v.rec.clone(), items: self.doc.items().to_vec(), events: v.events.clone(), edit: v.edit.clone(), media, quality: v.quality, fps: v.fps };
        let starter: crate::share_ui::Starter = Box::new(move |server, title, access| {
            // The job keeps the profile it starts with, whatever is switched to meanwhile.
            let pin = publish_ui::pin_for(&server);
            // Held until the job ends: its token is not ended at the site under it.
            let held = publish_ui::hold(&pin);
            crate::share_ui::ShareJob::spawn(share::Kind::Clip, move |r| {
                let _held = held;
                share_clip_job(&clip, &std::env::temp_dir(), &server, title, &access, || publish_ui::token_for_pin(&pin), r)
            })
        });
        let server = publish_ui::server();
        let blocked = crate::share_ui::blocked_reason(&server);
        let details = crate::share_ui::clip_details(duration_ms, width, height, media, None);
        let mut d = crate::share_ui::ShareDialog::open(share::Kind::Clip, server, &title, details, media == share::MediaType::Gif, blocked, starter);
        if let Some(t) = share::too_large(share::Kind::Clip, None, false, width, height, Some(duration_ms)) {
            d.refuse(&share::ShareError::TooLarge(t));
        }
        self.share = Some(d);
    }

    /// The dialog again after a publish or an update ended with `error` and nothing uploaded:
    /// see [`publish_ui::dialog_after_refusal`]. `left` is the dialog the upload was started from.
    fn bring_dialog_back(&mut self, left: Option<Picker>, error: String, updating: bool) {
        let title = self.title().to_string();
        let Some(v) = self.video.as_mut() else { return };
        if v.job.is_some() || v.picker.is_some() || v.publish_result.is_some() || v.site_dialog.is_some() {
            return;
        }
        let r = replaceable(v);
        v.picker = publish_ui::dialog_after_refusal(left, error, updating, |error| {
            let r = r?;
            let server = publish_ui::server();
            // The address was changed while the update ran: no Replace for the other site's tour.
            if !on_this_site(&r.key, &server) {
                return Some(Picker::open_for_edit_elsewhere(server, r.chapter_title.unwrap_or(title), r.target));
            }
            Some(Picker::open_after_failed_update(server, r.chapter_title.unwrap_or(title), r.target, error).with_logo_missing(r.logo_missing))
        });
    }

    /// "Update tour": replace the chapter this editor's recording was saved for, save the
    /// project over the saved one, then publish when the site says that is safe: see
    /// [`update_job`]. Only for an editor opened through Edit, only for its own tour and
    /// chapter on its own site, and never while another export runs. The project the editor
    /// was opened from stays its origin. `target.tour_title` is the tour's title as the site
    /// has it now: the result screen shows it, and the saved project takes it over.
    fn start_update(&mut self, title: String, target: EditTarget, before: Before) {
        self.start_update_with(title, target, before, publish_ui::pin(), &publish_ui::book(), Arc::new(publish_ui::token_for_pin));
    }

    /// [`Editor::start_update`] with the active profile's pin, the profiles and the token source
    /// handed in, so a test runs the real path without the shared state.
    fn start_update_with(&mut self, title: String, target: EditTarget, before: Before, pin: profiles::Pin, book: &profiles::Book, token: Arc<dyn Fn(&profiles::Pin) -> Result<String, String> + Send + Sync>) {
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        let Some(r) = replaceable(v) else { return };
        let pin_kept = pin.clone();
        let server = pin.server.clone();
        if r.gone || !on_this_site(&r.key, &server) || !is_chapter(&target, &r.key) || !owned_by(&r, book) {
            return;
        }
        let state = PublishState { profile: pin.profile.clone().unwrap_or_default(), rec: v.rec.clone(), items: self.doc.items().to_vec(), events: v.events.clone(), edit: v.edit.clone() };
        // A recording opened from a project is read from the project's folder, and may be saved
        // over it in place: the job holds the project open for as long as it runs, also past
        // this editor.
        let open = v.origin.as_ref().map(|o| o.open.clone());
        // A project that exists is rewritten whatever the keep setting says: left as it is it
        // would no longer match the tour. One that was never kept follows the setting.
        let keeping = if r.keep_always { keeping(true, Store::default_root()) } else { keeping_now() };
        let run = Run::begin();
        let worker = run.clone();
        let tour_title = target.tour_title.clone();
        // Held until the job ends, however it ends: its token is not ended at the site under it.
        let held = publish_ui::hold(&pin);
        let job = EncodeJob::spawn(move |progress| {
            let _held = held;
            let result = guarded_as(&worker, "Updating failed because of an internal error.", || update_job(&state, &title, &target, &server, || token(&pin), &keeping, &worker, progress));
            drop(state);
            drop(open);
            result
        });
        if let Some(v) = self.video.as_mut() {
            v.job = Some(ExportJob::Publish(job, run, tour_title, before, true));
            v.job_pin = Some(pin_kept);
        }
        self.notify("Updating tour… 0%");
    }

    /// Upload the tour, keep its recording on this computer when the setting says so, then
    /// publish the tour when `mode` says so: see [`publish_job`].
    fn start_publish(&mut self, title: String, target: Target, mode: Mode, tour_title: String, before: Before, access: Access) {
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        // The job keeps the profile it starts with, whatever is switched to meanwhile.
        let pin = publish_ui::pin();
        let pin_kept = pin.clone();
        let server = pin.server.clone();
        let state = PublishState { profile: pin.profile.clone().unwrap_or_default(), rec: v.rec.clone(), items: self.doc.items().to_vec(), events: v.events.clone(), edit: v.edit.clone() };
        // A recording opened from a saved project is read from that project's folder.
        let open = v.origin.as_ref().map(|o| o.open.clone());
        let keeping = keeping_now();
        let run = Run::begin();
        let worker = run.clone();
        let kept_as = tour_title.clone();
        let pending = Pending { profile: pin.profile.clone().unwrap_or_default(), server: server.clone(), chapter_title: title.clone() };
        // Held until the job ends, however it ends: its token is not ended at the site under it.
        let held = publish_ui::hold(&pin);
        let job = EncodeJob::spawn(move |progress| {
            let _held = held;
            let result = guarded(&worker, || publish_job_as(&state, &title, &kept_as, &server, || publish_ui::token_for_pin(&pin), &target, mode == Mode::PublishNow, &access, &keeping, &worker, progress));
            // The addresses go with the job.
            drop(access);
            // The recording is let go of before the project it may have been opened from.
            drop(state);
            drop(open);
            result
        });
        if let Some(v) = self.video.as_mut() {
            v.job = Some(ExportJob::Publish(job, run, tour_title, before, false));
            v.job_pin = Some(pin_kept);
            v.publishing_to = Some(pending);
        }
        self.notify("Publishing tour… 0%");
    }

    /// "Try again" after a publish that failed: publish the tour that is already uploaded.
    /// Nothing is written, uploaded or saved again, also after an update: the update is on
    /// the site and the project was saved when its commit answered.
    fn start_publish_only(&mut self, retry: publish_ui::Retry) {
        let Some(v) = self.video.as_ref() else { return };
        if v.job.is_some() {
            return;
        }
        let (tour_title, before, update) = (retry.title.clone(), retry.before.clone(), retry.update);
        // The tour is on the site of the profile the first publish went with, whatever is active now.
        let pin = v.job_pin.clone().unwrap_or_else(publish_ui::pin);
        let pin_kept = pin.clone();
        let server = pin.server.clone();
        let run = Run::begin();
        let worker = run.clone();
        worker.set_publishing(None);
        let held = publish_ui::hold(&pin);
        let job = EncodeJob::spawn(move |progress| {
            let _held = held;
            guarded(&worker, || {
                progress(1.0);
                let result = publish_ui::token_for_pin(&pin).and_then(|token| {
                    crate::publish::publish_tour(&server, &token, &retry.tour_id, |images| {
                        worker.set_publishing(images);
                        !worker.is_cancelled()
                    })
                });
                Ok(retry_done(&retry, result))
            })
        });
        if let Some(v) = self.video.as_mut() {
            v.job = Some(ExportJob::Publish(job, run, tour_title, before, update));
            v.job_pin = Some(pin_kept);
        }
        self.notify(publish_ui::publishing_message(None));
    }

    /// Ask a running publish to stop. The request in flight finishes first, so
    /// "Cancelling…" shows until it does.
    pub(crate) fn cancel_publish(&mut self) {
        let Some(v) = self.video.as_ref() else { return };
        if let Some(ExportJob::Publish(_, run, ..)) = &v.job {
            run.cancel();
            self.notify("Cancelling…");
        }
    }

    fn export_as(&mut self) {
        let Some(v) = self.video.as_ref() else { return };
        let ext = v.format.extension();
        let name = dest::new_path_with_extension(&self.dest, ext).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
        if let Some(p) = crate::dialogs::save_file(rfd::FileDialog::new().set_directory(&self.dest.output_dir).set_file_name(&name).add_filter(v.format.label(), &[ext]), "export as") {
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
        ui.label(RichText::new("Format").bold());
        // Read when the menu is drawn, not when the editor opened: the search for ffmpeg may
        // have ended since. Both are answers kept in memory; no program is started here.
        let ffmpeg = ffmpeg_available();
        v.format = usable_format(v.format, mythicshot_recording::ffmpeg::found());
        ui.horizontal(|ui| {
            for f in Format::ALL {
                let ok = !f.needs_ffmpeg() || ffmpeg;
                let r = ui.add_enabled(ok, egui::Button::selectable(v.format == f, f.label()));
                let r = if ok { r } else { r.on_disabled_hover_text("Needs ffmpeg installed and on the PATH") };
                if r.clicked() {
                    v.format = f;
                }
            }
        });
        ui.add_space(6.0);
        let format = v.format;
        output_controls(ui, v, format);
        let (tour_line, tour_ok) = match tour::plan_steps(&v.rec, &v.edit, &v.events) {
            Ok(steps) => (tour_steps_label(&steps), true),
            Err(e) => (e, false),
        };
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(!exporting, theme::primary_button("Export")).clicked() {
                self.start_export(None);
                ui.close();
            }
            if ui.add_enabled(!exporting, egui::Button::new("Export as…")).clicked() {
                self.export_as();
                ui.close();
            }
        });
        ui.separator();
        ui.label(RichText::new(crate::share_ui::EXPORT_HEADING).bold());
        if ui.add_enabled(!exporting, egui::Button::new(crate::share_ui::MENU_ENTRY)).clicked() {
            self.open_share_clip();
            ui.close();
        }
        ui.separator();
        ui.label(RichText::new("Tour").bold());
        ui.add(egui::Label::new(RichText::new(tour_line).color(theme::MUTED).size(12.0)).wrap());
        let publishing = self.video.as_ref().and_then(|v| v.job.as_ref()).and_then(|j| j.cancelling());
        let tour_running = self.video.as_ref().and_then(|v| v.job.as_ref()).and_then(|j| j.tour_cancelling());
        let blocked = publish_ui::publish_blocked(exporting, tour_ok, &publish_ui::server(), &publish_ui::token_status(), publish_ui::token_site().as_deref());
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
            let publish = theme::primary_button("Publish tour…");
            if ui.add_enabled(blocked.is_none(), publish).on_hover_text("Upload this tour to your server, as a public link or as a private draft").clicked() {
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
            ui.add(egui::Label::new(RichText::new(reason).color(theme::MUTED).size(12.0)).wrap());
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
        let frame = egui::Frame::new().fill(theme::SURFACE).inner_margin(egui::Margin::symmetric(8, 6));
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
                ui.label(RichText::new(format!("Frame {} / {} · {t:05.2} s", v.current + 1, v.rec.frames.len())).monospace().color(theme::MUTED));
                ui.separator();
                frame_buttons(ui, v, Place::Toolbar);
                ui.separator();
                speed_choice(ui, "speed", &mut v.edit.speed);
                switch(ui, Place::Toolbar, &mut v.edit.reverse, "Reverse");
                ui.separator();
                overlay_controls(ui, &mut v.edit.overlays, Place::Toolbar);
                let no_keys = !v.events.iter().any(|e| matches!(e.kind, mythicshot_editor::overlays::InputKind::KeyDown(_)));
                if no_keys && v.edit.overlays.keys != KeyStyle::Off {
                    theme::caution_line(ui, "No key presses were recorded", theme::SMALL_SIZE, theme::Fit::Truncate)
                        .on_hover_text("Key logging is off by default. Turn on Keys on the recording bar (or Record key presses in Preferences › Output) before recording.");
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
            painter.rect_filled(strip, CornerRadius::same(theme::RADIUS_SM), theme::GROUND);
            let kept = Rect::from_min_max(pos2(x_of(v.rec.frames[v.edit.trim.0].t_ms), strip.top()), pos2(x_of(frame_end(v.edit.trim.1)), strip.bottom()));
            painter.rect_filled(kept, 0.0, theme::BORDER);
            for (i, f) in v.rec.frames.iter().enumerate() {
                let x = x_of(f.t_ms);
                let color = if v.edit.deleted.contains(&i) { theme::RECORD } else { theme::BORDER_STRONG };
                painter.vline(x, strip.top() + 20.0..=strip.bottom() - 2.0, Stroke::new(1.0, color));
            }
            for x in [kept.left(), kept.right()] {
                let g = Rect::from_center_size(pos2(x, strip.center().y), vec2(6.0, strip.height() + 4.0));
                painter.rect_filled(g, 2.0, theme::ACCENT);
                if resp.hover_pos().is_some_and(|p| g.expand(4.0).contains(p)) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
            }

            // Annotation bars, in their own colour, with grips at both ends.
            if bars.is_empty() {
                painter.text(pos2(rect.left() + 4.0, band_top + LANE / 2.0), egui::Align2::LEFT_CENTER, "Annotations you add show here; drag their ends to set when they appear", egui::FontId::proportional(theme::SMALL_SIZE), theme::MUTED);
            }
            let hover = resp.hover_pos();
            for bar in &bars {
                let r = bar_rect(bar);
                let is_sel = selected == Some(bar.id);
                painter.rect_filled(r, 3.0, bar.color.gamma_multiply(if is_sel { 0.75 } else { 0.4 }));
                painter.rect_stroke(r, 3.0, Stroke::new(if is_sel { 2.0 } else { 1.0 }, if is_sel { theme::TEXT } else { bar.color }), StrokeKind::Inside);
                let label_clip = Rect::from_min_max(pos2(r.left() + GRIP + 3.0, r.top()), pos2(r.right() - GRIP - 3.0, r.bottom()));
                ui.painter_at(label_clip.intersect(rect)).text(pos2(label_clip.left(), r.center().y), egui::Align2::LEFT_CENTER, &bar.label, egui::FontId::proportional(theme::SMALL_SIZE), theme::TEXT);
                for x in [r.left() + GRIP / 2.0, r.right() - GRIP / 2.0] {
                    let g = Rect::from_center_size(pos2(x, r.center().y), vec2(GRIP, r.height()));
                    let hot = hover.is_some_and(|p| g.expand(2.0).contains(p));
                    painter.rect_filled(g, 3.0, if hot { theme::TEXT } else { theme::MUTED });
                    for dx in [-2.0, 2.0] {
                        painter.vline(g.center().x + dx, g.center().y - 4.0..=g.center().y + 4.0, Stroke::new(1.5, theme::SURFACE));
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
            painter.vline(head, rect.y_range(), Stroke::new(2.0, theme::TEXT));

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

    /// The Timeline view's panel: "This frame", "Playback", "Shown on the recording" and "Output",
    /// each control the very one of the controls row or the export panel. True when something changed.
    pub(crate) fn timeline_properties(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(v) = self.video.as_mut() else { return false };
        // A format that needs ffmpeg shows as GIF while there is none, as on export; the choice
        // itself is left as it is, and nothing is remembered by looking.
        let shown = usable_format(v.format, ffmpeg_found());
        let (before, output_before) = (timeline_settings(v), output_settings(v));
        crate::tour_cards::section(ui, "This frame");
        ui.label(RichText::new(format!("Frame {} of {}", v.current + 1, v.rec.frames.len())).color(theme::MUTED));
        ui.horizontal_wrapped(|ui| frame_buttons(ui, v, Place::Panel));
        crate::tour_cards::section(ui, "Playback");
        ui.horizontal(|ui| {
            row_label(ui, "Speed");
            speed_choice(ui, "speed-panel", &mut v.edit.speed);
        });
        switch(ui, Place::Panel, &mut v.edit.reverse, "Reverse");
        crate::tour_cards::section(ui, "Shown on the recording");
        overlay_controls(ui, &mut v.edit.overlays, Place::Panel);
        crate::tour_cards::section(ui, "Output");
        output_controls(ui, v, shown);
        let output_changed = output_settings(v) != output_before;
        let changed = output_changed || timeline_settings(v) != before;
        // Only a change to the output is the export panel's to remember, and only when it is made.
        if output_changed {
            self.remember_export();
        }
        changed
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

/// What is drawn on the recording: the Timeline view's controls row and the panel's "Shown on the
/// recording". The toolbar's choices name what they set ("Keys: Off"); the panel's lines say it before them.
fn overlay_controls(ui: &mut egui::Ui, o: &mut OverlayStyle, place: Place) {
    let (keys_salt, clicks_salt) = if place == Place::Toolbar { ("tl-keys", "tl-clicks") } else { ("tl-keys-panel", "tl-clicks-panel") };
    let shown = |name: &str, value: &str| if place == Place::Toolbar { format!("{name}: {value}") } else { value.to_string() };
    in_row(ui, place, "Keys", |ui| {
        egui::ComboBox::from_id_salt(keys_salt).popup_style(theme::menu_style.into()).width(118.0).selected_text(shown("Keys", key_style_label(o.keys))).show_ui(ui, |ui| {
            for k in [KeyStyle::Off, KeyStyle::Chips, KeyStyle::Keyboard] {
                ui.selectable_value(&mut o.keys, k, key_style_label(k));
            }
        });
    });
    in_row(ui, place, "Clicks", |ui| {
        egui::ComboBox::from_id_salt(clicks_salt).popup_style(theme::menu_style.into()).width(118.0).selected_text(shown("Clicks", click_style_label(o.clicks))).show_ui(ui, |ui| {
            for c in [ClickStyle::Off, ClickStyle::Ripple, ClickStyle::Mouse, ClickStyle::Both] {
                ui.selectable_value(&mut o.clicks, c, click_style_label(c));
            }
        });
    });
    switch(ui, place, &mut o.pointer, "Pointer");
    switch(ui, place, &mut o.halo, "Halo");
}

/// Trim to the shown frame from either end, and delete or restore it: the Timeline view's
/// controls row and the panel's "This frame".
fn frame_buttons(ui: &mut egui::Ui, v: &mut VideoState, place: Place) {
    if applied(ui.button("Start here").on_hover_text("Trim: keep from this frame"), place) {
        v.edit.trim.0 = v.current.min(v.edit.trim.1);
    }
    if applied(ui.button("End here").on_hover_text("Trim: keep up to this frame"), place) {
        v.edit.trim.1 = v.current.max(v.edit.trim.0);
    }
    let deleted = v.edit.deleted.contains(&v.current);
    if applied(ui.button(if deleted { "Restore frame" } else { "Delete frame" }), place) {
        if deleted {
            v.edit.deleted.remove(&v.current);
        } else {
            v.edit.deleted.insert(v.current);
        }
    }
}

/// The playback speeds: the Timeline view's controls row and the panel's "Speed".
fn speed_choice(ui: &mut egui::Ui, salt: &str, speed: &mut f32) {
    egui::ComboBox::from_id_salt(salt).popup_style(theme::menu_style.into()).width(64.0).selected_text(format!("{speed}×")).show_ui(ui, |ui| {
        for s in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
            ui.selectable_value(speed, s, format!("{s}×"));
        }
    });
}

/// The output's scale, the quality or fps of `format`, looping where it loops, and what comes out:
/// the export panel's and the panel's "Output".
fn output_controls(ui: &mut egui::Ui, v: &mut VideoState, format: Format) {
    ui.horizontal(|ui| {
        ui.label("Scale");
        for s in [100, 75, 50, 25] {
            if ui.add(egui::Button::selectable(v.edit.scale == s, format!("{s}%"))).clicked() {
                v.edit.scale = s;
            }
        }
    });
    match format {
        Format::Gif => {
            ui.add(egui::Slider::new(&mut v.quality, 50..=100).text("quality"));
        }
        Format::Mp4 | Format::Webm => {
            ui.add(egui::Slider::new(&mut v.fps, 10..=60).text("fps"));
        }
        Format::Apng => {}
    }
    if matches!(format, Format::Gif | Format::Apng) {
        ui.checkbox(&mut v.loop_forever, "Loop forever");
    }
    let (w, h) = video::output_size(&v.rec, &v.edit);
    let (frames, end) = video::plan(&v.rec, &v.edit);
    ui.label(RichText::new(format!("{w} × {h} · {} frames · {:.1} s", frames.len(), end as f64 / 1000.0)).color(theme::MUTED).size(12.0));
}

/// What the Timeline view's panel sets, to tell whether a frame of it changed something.
type TimelineSettings = ((usize, usize), std::collections::BTreeSet<usize>, f32, bool, OverlayStyle);

fn timeline_settings(v: &VideoState) -> TimelineSettings {
    let e = &v.edit;
    (e.trim, e.deleted.clone(), e.speed, e.reverse, e.overlays)
}

/// What the panel's "Output" sets: the export choices that are remembered for the next recording.
fn output_settings(v: &VideoState) -> (u32, u8, u32, bool) {
    (v.edit.scale, v.quality, v.fps, v.loop_forever)
}

#[cfg(test)]
thread_local! {
    /// What a test says the search for ffmpeg found, in place of the real answer.
    static FFMPEG_FOR_TEST: std::cell::Cell<Option<Found>> = const { std::cell::Cell::new(None) };
}

/// What the search for ffmpeg found, as the Timeline view's panel reads it.
fn ffmpeg_found() -> Found {
    #[cfg(test)]
    if let Some(found) = FFMPEG_FOR_TEST.with(|f| f.get()) {
        return found;
    }
    mythicshot_recording::ffmpeg::found()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::Finished;
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
    fn closing_the_editor_cancels_a_publish_and_no_other_export() {
        let done = || Ok(PublishDone { finished: Finished { tour_id: "tour_aaaaaaaaaaaaaaaaaaaaaaaa".into(), chapter_id: String::new(), steps: 1, outcome: crate::publish::Outcome::Draft }, kept: Kept::Off });
        // Runs of their own, not in the shared list that quitting cancels.
        let run = Arc::new(Run::default());
        let publish = ExportJob::Publish(EncodeJob::spawn(move |_| done()), run.clone(), "Demo".into(), Before::new_tour(), false);
        assert_eq!(publish.cancelling(), Some(false));
        publish.abandon();
        assert!(run.is_cancelled());
        assert_eq!(publish.cancelling(), Some(true));
        // A tour export is left to finish: its run is not touched.
        let run = Arc::new(Run::default());
        let summary = || Ok(TourSummary { path: PathBuf::from("x"), steps: 1, bytes: 1, squeezed: 0 });
        let export = ExportJob::Tour(EncodeJob::spawn(move |_| summary()), TourOutput::Folder, run.clone());
        export.abandon();
        assert!(!run.is_cancelled());
    }

    #[test]
    fn a_finished_publish_hands_the_result_screen_what_the_tour_was() {
        let was = Before { public: true, link: None, steps: Some(14) };
        let done = PublishDone { finished: Finished { tour_id: "tour_aaaaaaaaaaaaaaaaaaaaaaaa".into(), chapter_id: String::new(), steps: 2, outcome: crate::publish::Outcome::Draft }, kept: Kept::Saved { left_out: vec!["the cover logo".into()], over: None } };
        let sent = done.clone();
        let job = ExportJob::Publish(EncodeJob::spawn(move |_| Ok(sent)), Arc::new(Run::default()), "Customer onboarding".into(), was.clone(), false);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut result = None;
        while result.is_none() && Instant::now() < deadline {
            result = job.take_result();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        match result {
            Some(Ok(Exported::Published(got, title, before, false))) => {
                assert_eq!((got, title.as_str(), &before), (done, "Customer onboarding", &was));
                assert_eq!(publish_ui::result_status(&Finished { tour_id: String::new(), chapter_id: String::new(), steps: 2, outcome: crate::publish::Outcome::Draft }, &before, false), "Tour uploaded as a draft: 16 steps; the public version is unchanged");
            }
            _ => panic!("the publish did not finish as a published result"),
        }
    }

    #[test]
    fn publishing_shows_its_progress_and_then_cancelling() {
        assert_eq!(progress_message(true, Some(false), false, None, false, 0.4), "Publishing tour… 40%");
        assert_eq!(progress_message(true, Some(true), false, None, false, 0.4), "Cancelling…");
        assert_eq!(progress_message(true, None, false, None, false, 0.25), "Exporting tour… 25%");
        assert_eq!(progress_message(true, None, true, None, false, 0.25), "Cancelling…", "a tour export being cancelled");
        assert_eq!(progress_message(false, None, false, None, false, 0.5), "Exporting… 50%");
        // After the upload, the publish step shows the server's count instead of a percentage.
        assert_eq!(progress_message(true, Some(false), false, Some(None), false, 1.0), "Publishing…");
        assert_eq!(progress_message(true, Some(false), false, Some(Some((12, 22))), false, 1.0), "Publishing… 12 of 22 images");
        assert_eq!(progress_message(true, Some(true), false, Some(Some((12, 22))), false, 1.0), "Cancelling…");
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

    // ---- Keeping the recording at publish, and reopening it ----

    use crate::project::{LoadError, ProjectInfo};
    use crate::publish::tests::{CH, LIVE, PUBLISH, Stub, TOKEN, TOUR, platform, publish_stub, reply, stub};
    use crate::publish::{CANCELLED, Outcome};
    use crate::settings::Settings;
    use crate::tour::{Card, Cover, StepExtras};
    use mythicshot_editor::Item;
    use mythicshot_editor::overlays::{InputKind, MouseButton};

    const TITLE: &str = "Chapter one";
    const TOUR_TITLE: &str = "Customer onboarding";

    /// A real recording of six frames of 200 x 160 that owns its folder `<root>/rec`, as the recorder leaves one.
    fn recorded(root: &Path) -> Arc<Recording> {
        let dir = root.join("rec");
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(Recording::synthetic_in(&dir, 200, 160, &[0, 500, 1000, 1500, 2000, 2500], 3000).unwrap())
    }

    fn left(t_ms: u64, x: i32, y: i32) -> InputEvent {
        InputEvent { t_ms, kind: InputKind::MouseDown(MouseButton::Left, x, y) }
    }

    /// An editor's state worth keeping: a trim, a crop, three steps of which one is removed, a
    /// cover, a guide card on the first step, a rectangle on some frames and a note pinned to
    /// the first step; three clicks and a key press.
    fn state(root: &Path) -> PublishState {
        let rec = recorded(root);
        let events = vec![left(750, 40, 30), InputEvent { t_ms: 900, kind: InputKind::KeyDown("Ctrl".into()) }, left(1250, 60, 50), left(2250, 120, 90)];
        let mut edit = Edit::new(&rec, OverlayStyle::default());
        edit.trim = (1, 5);
        edit.crop = Some(Bounds::new(pt(20.0, 10.0), pt(180.0, 150.0)));
        let steps = tour::plan_steps(&rec, &edit, &events).unwrap();
        assert!(steps.len() >= 3, "{steps:?}");
        edit.tour_removed.insert(steps[1].src);
        let card = Card { title: "Open the menu".into(), body: "It is at the top left.".into(), next_label: "Next".into(), anchor: Default::default(), point: None, placement: Default::default() };
        edit.tour_doc.steps.insert(steps[0].src, StepExtras { card: Some(card), ..Default::default() });
        edit.tour_doc.cover = Some(Cover { title: "Welcome".into(), body: "A short tour.".into(), button: "Start".into(), logo: None });
        let items = vec![
            Item { id: 1, shape: Shape::Rect(Bounds::new(pt(30.0, 20.0), pt(90.0, 70.0))), style: Style::default(), frames: Some((1, 3)), pin: None },
            Item { id: 2, shape: Shape::Text { at: pt(40.0, 80.0), text: "Mind the gap".into() }, style: Style::default(), frames: None, pin: Some(steps[0].src as u32) },
        ];
        PublishState { profile: String::new(), rec, items, events, edit }
    }

    fn store_in(root: &Path) -> Store {
        Store::at(root.join("projects"))
    }

    /// The project a publish to `stub` makes: its address as the address rule writes it, and the ids the stub answers.
    fn key_at(stub: &Stub) -> ProjectKey {
        ProjectKey { server: crate::publish::check_server(&stub.base).unwrap(), tour_id: TOUR.into(), chapter_id: CH.into() }
    }

    /// Run the publish job against `stub`, as the editor's job thread does, with the test token.
    fn publish_to(stub: &Stub, state: &PublishState, publish_now: bool, keeping: &Keeping) -> Result<PublishDone, String> {
        publish_job(state, TITLE, TOUR_TITLE, &stub.base, || Ok(TOKEN.to_string()), &Target::New, publish_now, keeping, &Arc::new(Run::default()), &|_| {})
    }

    fn project_text(info: &ProjectInfo) -> String {
        std::fs::read_to_string(info.dir.join("project.json")).unwrap()
    }

    /// The editor a recording opens in, with nothing remembered.
    fn editor_of(state: &PublishState) -> Editor {
        let s = Settings::default();
        Editor::new_video(state.rec.clone(), state.events.clone(), state.items.clone(), TITLE.into(), Default::default(), s.dest_context(TITLE), OverlayStyle::default(), s.editor.clone()).unwrap()
    }

    /// "Add to site" is one of the dialogs that makes the editor inert: behind it the editor stays solid, undimmed and inert.
    #[test]
    fn the_editor_behind_add_to_site_stays_solid_undimmed_and_inert() {
        let tmp = tempfile::tempdir().unwrap();
        // Each editor takes its recording's folder with it, so each gets a recording of its own.
        let made = std::cell::Cell::new(0);
        let make = || {
            made.set(made.get() + 1);
            let rec = recorded(&tmp.path().join(made.get().to_string()));
            let s = Settings::default();
            Editor::new_video(rec, Vec::new(), Vec::new(), TITLE.into(), Default::default(), s.dest_context(TITLE), OverlayStyle::default(), s.editor.clone()).unwrap()
        };
        crate::editor_ui::dialog_checks::assert_inert_and_solid(make, |ed| ed.open_site_dialog(), |ed| ed.site_dialog_open());
        // The video editor paints its whole window opaque, alone and with the dialog open, which is opaque too.
        {
            let ctx = crate::theme::test_context();
            let mut ed = make();
            let run = |ed: &mut Editor| {
                let input = egui::RawInput { time: Some(ctx.input(|i| i.time) + 1.0), ..crate::editor_ui::dialog_checks::input(vec![]) };
                let mut out = ctx.run_ui(input, |ui| ed.ui(ui));
                out.textures_delta.clear();
                out
            };
            run(&mut ed);
            crate::theme::checks::assert_window_opaque(&run(&mut ed), crate::editor_ui::dialog_checks::WINDOW, "the video editor");
            ed.open_site_dialog();
            run(&mut ed);
            let out = run(&mut ed);
            crate::theme::checks::assert_window_opaque(&out, crate::editor_ui::dialog_checks::WINDOW, "the video editor with add to site open");
            crate::theme::checks::assert_dialog_opaque(&out, "add to site over the video editor");
        }
        // A click outside it closes nothing.
        let ctx = crate::theme::test_context();
        let mut ed = make();
        ed.open_site_dialog();
        crate::editor_ui::dialog_checks::frame(&mut ed, &ctx, vec![]);
        crate::editor_ui::dialog_checks::click(&mut ed, &ctx, egui::pos2(10.0, 810.0));
        assert!(ed.site_dialog_open());
    }

    /// Filled in pixel by pixel at 125% scaling, the video editor lets nothing behind its window
    /// show: with its timeline, and in the Tour view with the Tour settings panel open.
    #[test]
    fn the_video_editor_its_timeline_and_tour_settings_are_opaque_pixel_by_pixel() {
        let tmp = tempfile::tempdir().unwrap();
        let make = |name: &str| {
            let s = Settings::default();
            Editor::new_video(recorded(&tmp.path().join(name)), Vec::new(), Vec::new(), TITLE.into(), Default::default(), s.dest_context(TITLE), OverlayStyle::default(), s.editor.clone()).unwrap()
        };
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let mut ed = make("timeline");
        assert_eq!(crate::snapshot::uncovered(&crate::theme::test_context(), size, 1.25, 3, |ui| ed.ui(ui)), 0, "the video editor with its timeline");
        let mut ed = make("tour");
        ed.test_tour_view();
        ed.video.as_mut().unwrap().tour.settings.open = true;
        let ctx = crate::theme::test_context();
        assert_eq!(crate::snapshot::uncovered(&ctx, size, 1.25, 3, |ui| ed.ui(ui)), 0, "the video editor with Tour settings open");
        // And the panel was there to be checked: 304 points wide (a point of it the separator), on surface.
        let mut out = ctx.run_ui(crate::editor_ui::dialog_checks::input(vec![]), |ui| ed.ui(ui));
        out.textures_delta.clear();
        let fills = crate::theme::checks::fills(&out);
        assert!(fills.iter().any(|(r, c, _)| *c == crate::theme::SURFACE && (r.width() - 304.0).abs() <= 2.0), "the Tour settings panel was drawn: {:?}", fills.iter().filter(|(r, ..)| r.width() > 200.0).collect::<Vec<_>>());
    }

    /// Every label of the video editor fits what holds it: the timeline and its controls row
    /// (with the caution that no key presses were recorded), and in the Tour view the step strip
    /// and the Tour settings panel, 304 points wide.
    #[test]
    fn the_video_editors_labels_fit_its_timeline_and_tour_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let frames = |ed: &mut Editor| {
            let mut last = None;
            for _ in 0..3 {
                let input = egui::RawInput { time: Some(ctx.input(|i| i.time) + 1.0), ..crate::editor_ui::dialog_checks::input(vec![]) };
                let mut out = ctx.run_ui(input, |ui| ed.ui(ui));
                out.textures_delta.clear();
                last = Some(out);
            }
            last.unwrap()
        };
        let s = Settings::default();
        let mut ed = Editor::new_video(recorded(&tmp.path().join("timeline")), Vec::new(), Vec::new(), TITLE.into(), Default::default(), s.dest_context(TITLE), OverlayStyle::default(), s.editor.clone()).unwrap();
        ed.video.as_mut().unwrap().edit.overlays.keys = KeyStyle::Chips;
        let out = frames(&mut ed);
        let all: String = out.shapes.iter().map(|c| format!("{:?}", c.shape)).collect();
        assert!(all.contains("No key presses were recorded"), "the controls row shows its caution");
        assert!(crate::theme::checks::assert_texts_fit(&out, "the video editor's timeline") >= 5);
        let mut ed = Editor::new_video(recorded(&tmp.path().join("tour")), Vec::new(), Vec::new(), TITLE.into(), Default::default(), s.dest_context(TITLE), OverlayStyle::default(), s.editor.clone()).unwrap();
        ed.test_tour_view();
        ed.video.as_mut().unwrap().tour.settings.open = true;
        let out = frames(&mut ed);
        assert!(crate::theme::checks::assert_texts_fit(&out, "the Tour view with Tour settings open") >= 5);
    }

    // ---- The menu bar, the Properties panel and the Tour view's strip ----
    use crate::menu_bar::Entry;

    /// A recording editor with `prefs`, its keys the default ones: three clicks, so its tour has
    /// three steps and the last frame.
    fn video_editor_with(dir: &Path, prefs: crate::settings::EditorPrefs) -> Editor {
        let s = Settings::default();
        let events = vec![left(750, 40, 30), left(1250, 60, 50), left(2250, 120, 90)];
        let mut ed = Editor::new_video(recorded(dir), events, Vec::new(), TITLE.into(), Default::default(), s.dest_context(TITLE), OverlayStyle::default(), prefs).unwrap();
        ed.keys = crate::hotkeys::EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        ed
    }

    /// One frame of `ed` in a window `size` points big.
    fn sized(ed: &mut Editor, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        crate::editor_ui::tests::sized_frame(ed, ctx, size, events)
    }

    /// The Properties panel as drawn: the 304-point column of surface at the right, if it is there.
    fn properties_panel(out: &egui::FullOutput) -> Option<egui::Rect> {
        crate::theme::checks::fills(out).into_iter().find(|(r, c, _)| *c == crate::theme::SURFACE && (r.width() - 304.0).abs() <= 2.0 && r.height() > 200.0).map(|(r, ..)| r)
    }

    /// Where `text` is drawn inside `area`, if it is.
    fn text_in(out: &egui::FullOutput, text: &str, area: egui::Rect) -> Option<egui::Rect> {
        crate::share_ui::tests::shapes_of(out).into_iter().find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == text && area.contains(t.pos + t.galley.rect.center().to_vec2()) => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        })
    }

    #[test]
    fn the_recording_editors_menus_export_and_publish_and_offer_properties_in_both_views() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ed = video_editor_with(&tmp.path().join("rec"), Default::default());
        let find = |ed: &Editor, e: Entry| crate::menu_bar::tests::find(&crate::menu_bar::menus(&ed.menu_state()), e);
        // It exports from its export panel, and has no save of its own.
        assert!(!find(&ed, Entry::Save).enabled && find(&ed, Entry::Export).enabled);
        // "Publish tour…" is usable exactly when the export panel's button is: never without a site and token, as in a test run.
        assert_eq!(find(&ed, Entry::PublishTour).enabled, ed.can_publish_tour());
        assert!(!ed.can_publish_tour());
        // The Timeline view has its panel, closed at first: the entry is usable and unchecked.
        assert_eq!((find(&ed, Entry::Properties).enabled, find(&ed, Entry::Properties).checked), (true, Some(false)));
        // In the Tour view it is usable, and checked while the panel is open.
        ed.set_view(VideoView::Tour);
        assert_eq!((find(&ed, Entry::Properties).enabled, find(&ed, Entry::Properties).checked), (true, Some(true)));
        assert!(find(&ed, Entry::Export).enabled && !find(&ed, Entry::Save).enabled);
    }

    // ---- The Timeline view's Properties panel ----
    use crate::editor_ui::tests::{assert_in_order, button_lit, in_panel, panel_headings, panel_of, panel_texts};

    /// The Properties button and the Edit menu open and close the Timeline view's panel, the button
    /// lit while it is open, and its own close button closes it. Whether it is open is remembered for
    /// the next Timeline view, in this recording and the next, apart from the Tour view's panel.
    #[test]
    fn the_properties_button_and_the_edit_menu_open_and_close_the_timeline_views_panel() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let mut ed = video_editor_with(&tmp.path().join("a"), Default::default());
        assert!(!ed.properties_open() && !ed.prefs.timeline_properties_open, "closed by default");
        sized(&mut ed, &ctx, size, vec![]);
        let out = sized(&mut ed, &ctx, size, vec![]);
        assert!(properties_panel(&out).is_none() && !button_lit(&out));
        let at = crate::share_ui::tests::text_rect(&out, crate::editor_ui::PROPERTIES).expect("the button");
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), at.center());
        assert!(ed.properties_open() && ed.prefs.timeline_properties_open && ed.prefs_changed);
        let out = sized(&mut ed, &ctx, size, vec![]);
        let panel = properties_panel(&out).expect("open, docked at the right");
        assert!((panel.right() - size.x).abs() <= 1.0, "{panel:?}");
        assert!(button_lit(&out), "the button is lit while the panel is open");
        // The image editor's and the Tour view's settings are their own.
        assert!(!ed.prefs.image_properties_open && ed.prefs.tour_properties_open);
        // To the Tour view and back: its own panel there, this one here again.
        ed.set_view(VideoView::Tour);
        assert!(ed.properties_open(), "the Tour view's panel, open by its own setting");
        ed.toggle_properties();
        assert!(!ed.prefs.tour_properties_open && ed.prefs.timeline_properties_open);
        ed.set_view(VideoView::Timeline);
        assert!(ed.properties_open(), "still open in the Timeline view");
        let next = video_editor_with(&tmp.path().join("b"), crate::editor_ui::tests::restarted(&ed.prefs));
        assert!(next.properties_open(), "open in the next recording's Timeline view, after a restart");

        // Closed with the Edit menu, and opened with it again.
        crate::menu_bar::tests::choose(&mut |events| sized(&mut ed, &ctx, size, events), "Edit", "Properties");
        assert!(!ed.properties_open() && !ed.prefs.timeline_properties_open);
        assert!(properties_panel(&sized(&mut ed, &ctx, size, vec![])).is_none());
        let next = video_editor_with(&tmp.path().join("c"), crate::editor_ui::tests::restarted(&ed.prefs));
        assert!(!next.properties_open(), "closed after a restart");
        crate::menu_bar::tests::choose(&mut |events| sized(&mut ed, &ctx, size, events), "Edit", "Properties");
        assert!(ed.properties_open() && ed.prefs.timeline_properties_open);

        // Closed with its own close button, at the right of its heading.
        sized(&mut ed, &ctx, size, vec![]);
        let out = sized(&mut ed, &ctx, size, vec![]);
        let panel = properties_panel(&out).unwrap();
        let heading = text_in(&out, crate::editor_ui::PROPERTIES, panel).unwrap();
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), egui::pos2(panel.right() - 12.0 - 11.0, heading.center().y));
        assert!(!ed.properties_open() && !ed.prefs.timeline_properties_open);
    }

    /// The Timeline view's panel: exactly its four sections in order, each control in order; the
    /// frame's button says what it does to the shown frame, and the output's lines follow the format.
    #[test]
    fn the_timeline_views_panel_shows_the_frame_playback_overlays_and_output_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = egui::vec2(1280.0, 1600.0);
        let prefs = crate::settings::EditorPrefs { timeline_properties_open: true, ..Default::default() };
        let mut ed = video_editor_with(&tmp.path().join("rec"), prefs);
        ed.video.as_mut().unwrap().format = Format::Gif;
        let frames = |ed: &mut Editor| {
            for _ in 0..2 {
                sized(ed, &ctx, size, vec![]);
            }
            sized(ed, &ctx, size, vec![])
        };
        let out = frames(&mut ed);
        let panel = panel_of(&out).expect("open");
        assert_eq!(panel_headings(&out, panel), ["Properties", "This frame", "Playback", "Shown on the recording", "Output"]);
        let v = ed.video.as_ref().unwrap();
        let count = format!("Frame 1 of {}", v.rec.frames.len());
        let (w, h) = video::output_size(&v.rec, &v.edit);
        let (planned, end) = video::plan(&v.rec, &v.edit);
        let line = format!("{w} × {h} · {} frames · {:.1} s", planned.len(), end as f64 / 1000.0);
        let order = ["Properties", "This frame", count.as_str(), "Start here", "Playback", "Speed", "Reverse", "Shown on the recording", "Keys", "Clicks", "Pointer", "Halo", "Output", "Scale", "quality", "Loop forever", line.as_str()];
        assert_in_order(&out, panel, &order, "the Timeline view's panel");
        for t in ["End here", "Delete frame", "100%", "75%", "50%", "25%"] {
            assert!(in_panel(&out, panel, t).is_some(), "{t}");
        }
        assert!(in_panel(&out, panel, "Restore frame").is_none() && in_panel(&out, panel, "fps").is_none());
        // A deleted frame: "Restore frame", in the panel and in the controls row alike.
        let current = ed.video.as_ref().unwrap().current;
        ed.video.as_mut().unwrap().edit.deleted.insert(current);
        let out = frames(&mut ed);
        let panel = panel_of(&out).unwrap();
        assert!(in_panel(&out, panel, "Restore frame").is_some() && in_panel(&out, panel, "Delete frame").is_none());
        assert_eq!(crate::share_ui::tests::shapes_of(&out).into_iter().filter(|s| matches!(s, egui::Shape::Text(t) if t.galley.text() == "Restore frame")).count(), 2, "in both places");
        // APNG: no quality slider, and it loops.
        ed.video.as_mut().unwrap().format = Format::Apng;
        let out = frames(&mut ed);
        let panel = panel_of(&out).unwrap();
        assert!(in_panel(&out, panel, "quality").is_none() && in_panel(&out, panel, "fps").is_none() && in_panel(&out, panel, "Loop forever").is_some());
        // A video format: its fps and no looping, when ffmpeg is there to make it (else it gives way to GIF, as the export panel does).
        ed.video.as_mut().unwrap().format = Format::Mp4;
        let out = frames(&mut ed);
        let panel = panel_of(&out).unwrap();
        if ed.video.as_ref().unwrap().format == Format::Mp4 {
            assert!(in_panel(&out, panel, "fps").is_some() && in_panel(&out, panel, "Loop forever").is_none() && in_panel(&out, panel, "quality").is_none());
        } else {
            assert!(in_panel(&out, panel, "quality").is_some() && in_panel(&out, panel, "fps").is_none());
        }
    }

    /// A change made in the Timeline view's panel is the change its controls row or export panel
    /// makes: the same state, shown in both places, remembered as they remember it, and like theirs
    /// no step of Undo.
    #[test]
    fn a_change_in_the_timeline_views_panel_is_the_controls_rows_and_undoes_alike() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let prefs = crate::settings::EditorPrefs { timeline_properties_open: true, ..Default::default() };
        let mut ed = video_editor_with(&tmp.path().join("rec"), prefs);
        let undo_before = (ed.doc.can_undo(), ed.doc.can_redo());
        let frame = |ed: &mut Editor| {
            sized(ed, &ctx, size, vec![]);
            sized(ed, &ctx, size, vec![])
        };
        // The text in the panel, or the same text outside it (the controls row).
        let at = |ed: &mut Editor, text: &str, panel_side: bool| {
            let out = frame(ed);
            let panel = panel_of(&out).expect("open");
            if panel_side {
                in_panel(&out, panel, text).unwrap_or_else(|| panic!("the panel's {text}")).center()
            } else {
                crate::share_ui::tests::shapes_of(&out).into_iter().find_map(|s| match s {
                    egui::Shape::Text(t) if t.galley.text() == text && !panel.contains(t.pos) && t.pos.y > size.y / 2.0 => Some(t.galley.rect.translate(t.pos.to_vec2()).center()),
                    _ => None,
                }).unwrap_or_else(|| panic!("the controls row's {text}"))
            }
        };
        let click = |ed: &mut Editor, text: &str, panel_side: bool| {
            let p = at(ed, text, panel_side);
            crate::menu_bar::tests::click_at(&mut |events| sized(ed, &ctx, size, events), p);
        };
        let v = |ed: &Editor| ed.video.as_ref().unwrap().edit.clone();

        // Reverse: on in the panel, off again in the controls row.
        click(&mut ed, "Reverse", true);
        assert!(v(&ed).reverse, "the panel's Reverse sets the recording's");
        click(&mut ed, "Reverse", false);
        assert!(!v(&ed).reverse, "and the controls row's reads and sets the same");
        // Pointer and Halo, likewise.
        let (pointer, halo) = (v(&ed).overlays.pointer, v(&ed).overlays.halo);
        click(&mut ed, "Pointer", true);
        click(&mut ed, "Halo", false);
        assert_eq!((v(&ed).overlays.pointer, v(&ed).overlays.halo), (!pointer, !halo));
        click(&mut ed, "Pointer", false);
        click(&mut ed, "Halo", true);
        assert_eq!((v(&ed).overlays.pointer, v(&ed).overlays.halo), (pointer, halo));
        // Trim from the panel at the shown frame, then from the controls row.
        let last = ed.video.as_ref().unwrap().rec.frames.len() - 1;
        ed.video.as_mut().unwrap().current = 2;
        click(&mut ed, "Start here", true);
        assert_eq!(v(&ed).trim, (2, last));
        ed.video.as_mut().unwrap().current = 4.min(last);
        click(&mut ed, "End here", false);
        assert_eq!(v(&ed).trim, (2, 4.min(last)));
        // Delete the shown frame in the panel; the controls row offers to restore it.
        ed.video.as_mut().unwrap().current = 3;
        click(&mut ed, "Delete frame", true);
        assert!(v(&ed).deleted.contains(&3));
        click(&mut ed, "Restore frame", false);
        assert!(!v(&ed).deleted.contains(&3));
        // The output's scale: the export panel's setting, remembered for the next recording as it remembers it.
        ed.prefs_changed = false;
        click(&mut ed, "50%", true);
        assert_eq!((v(&ed).scale, ed.prefs.export_scale, ed.prefs_changed), (50, 50, true));
        // None of it is a step of Undo, as none of it is from the controls row.
        assert_eq!((ed.doc.can_undo(), ed.doc.can_redo()), undo_before);
        // The frame of a change asks for another, so the controls row drawn before the panel shows it at once.
        let p = at(&mut ed, "Reverse", true);
        let button = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        sized(&mut ed, &ctx, size, vec![egui::Event::PointerMoved(p), button(true)]);
        let released = sized(&mut ed, &ctx, size, vec![button(false)]);
        assert!(v(&ed).reverse && released.viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|o| o.repaint_delay == std::time::Duration::ZERO));
    }

    /// An open panel only shows the export choices: a recording whose scale, format, quality, fps and
    /// looping differ from the saved ones leaves the saved ones as they are, frame after frame, also
    /// with a video format saved while ffmpeg is reported missing (shown as GIF, kept as MP4). A
    /// change to an output control in the panel is saved, once.
    #[test]
    fn an_open_timeline_panel_saves_the_export_choices_only_when_one_is_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let prefs = crate::settings::EditorPrefs { timeline_properties_open: true, ..Default::default() };
        let mut ed = video_editor_with(&tmp.path().join("own"), prefs.clone());
        let v = ed.video.as_mut().unwrap();
        (v.edit.scale, v.format, v.quality, v.fps, v.loop_forever) = (50, Format::Apng, 60, 15, false);
        ed.prefs_changed = false;
        for _ in 0..6 {
            sized(&mut ed, &ctx, size, vec![]);
        }
        assert!(properties_panel(&sized(&mut ed, &ctx, size, vec![])).is_some(), "the panel is open");
        assert_eq!(ed.prefs, prefs, "the saved export choices are untouched");
        assert!(!ed.prefs_changed);

        // MP4 saved, ffmpeg reported missing: shown as GIF, kept as MP4, nothing saved.
        FFMPEG_FOR_TEST.with(|f| f.set(Some(Found::None)));
        let saved = crate::settings::EditorPrefs { export_format: "mp4".into(), ..prefs.clone() };
        let mut ed = video_editor_with(&tmp.path().join("mp4"), saved.clone());
        ed.video.as_mut().unwrap().format = Format::Mp4;
        ed.prefs_changed = false;
        for _ in 0..6 {
            sized(&mut ed, &ctx, size, vec![]);
        }
        let out = sized(&mut ed, &ctx, size, vec![]);
        let panel = panel_of(&out).unwrap();
        assert!(in_panel(&out, panel, "quality").is_some() && in_panel(&out, panel, "fps").is_none(), "shown as GIF");
        assert_eq!(ed.video.as_ref().unwrap().format, Format::Mp4, "the recording's choice is kept");
        assert_eq!((ed.prefs.export_format.as_str(), ed.prefs_changed), ("mp4", false), "and the saved one");
        // A click on an output control saves the choices, once, with the format as chosen.
        let at = in_panel(&out, panel, "50%").unwrap().center();
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), at);
        assert!(ed.prefs_changed && ed.prefs.export_scale == 50 && ed.prefs.export_format == "mp4", "saved: {:?}", ed.prefs);
        ed.prefs_changed = false;
        for _ in 0..6 {
            sized(&mut ed, &ctx, size, vec![]);
        }
        assert!(!ed.prefs_changed, "once");
        FFMPEG_FOR_TEST.with(|f| f.set(None));
    }

    /// "Delete frame" in the panel, clicked once, deletes the frame once: a later Space or Enter does
    /// not click it again.
    #[test]
    fn a_frame_deleted_in_the_panel_stays_deleted_whatever_keys_follow() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let prefs = crate::settings::EditorPrefs { timeline_properties_open: true, ..Default::default() };
        let mut ed = video_editor_with(&tmp.path().join("rec"), prefs);
        ed.video.as_mut().unwrap().current = 2;
        sized(&mut ed, &ctx, size, vec![]);
        let out = sized(&mut ed, &ctx, size, vec![]);
        let at = in_panel(&out, panel_of(&out).unwrap(), "Delete frame").unwrap().center();
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), at);
        assert!(ed.video.as_ref().unwrap().edit.deleted.contains(&2));
        assert_eq!(ctx.memory(|m| m.focused()), None, "the button let go of the keyboard");
        for key in [egui::Key::Space, egui::Key::Enter] {
            for pressed in [true, false] {
                sized(&mut ed, &ctx, size, vec![egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }]);
            }
        }
        let v = ed.video.as_mut().unwrap();
        v.playing = None;
        assert_eq!(v.edit.deleted.iter().copied().collect::<Vec<_>>(), [2], "deleted once, not restored by a key");
    }

    /// Every label of the Timeline view's panel fits it at 100%, in the editor's own window and at
    /// the narrowest the editor allows.
    #[test]
    fn the_timeline_views_panel_labels_fit_it() {
        let tmp = tempfile::tempdir().unwrap();
        for (i, size) in [crate::editor_ui::dialog_checks::WINDOW.size(), egui::vec2(crate::editor_ui::EDITOR_MIN_SIZE[0], crate::editor_ui::EDITOR_MIN_SIZE[1])].into_iter().enumerate() {
            for format in [Format::Gif, Format::Apng] {
                let ctx = crate::theme::test_context();
                let prefs = crate::settings::EditorPrefs { timeline_properties_open: true, ..Default::default() };
                let mut ed = video_editor_with(&tmp.path().join(format!("{i}{format:?}")), prefs);
                ed.video.as_mut().unwrap().format = format;
                // The longest choices shown.
                ed.video.as_mut().unwrap().edit.overlays.keys = KeyStyle::Keyboard;
                ed.video.as_mut().unwrap().edit.overlays.clicks = ClickStyle::Both;
                for _ in 0..2 {
                    sized(&mut ed, &ctx, size, vec![]);
                }
                let out = sized(&mut ed, &ctx, size, vec![]);
                let what = format!("the Timeline view's panel, {format:?}, {size:?}");
                let panel = panel_of(&out).unwrap_or_else(|| panic!("{what}: open"));
                let texts = panel_texts(&out, panel);
                assert!(texts.len() >= 10, "{what}: {texts:?}");
                for (t, r) in &texts {
                    assert!(panel.expand(0.5).contains_rect(*r), "{what}: {t:?} at {r:?} sticks out of the panel {panel:?}");
                }
                assert!(crate::theme::checks::assert_texts_fit(&out, &what) >= 5);
            }
        }
    }

    /// "Export as…" asks for the system's save dialog, which a test build never opens.
    #[test]
    fn export_as_never_opens_a_dialog() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ed = video_editor_with(&tmp.path().join("rec"), Default::default());
        mythicshot_destinations::stubs::take();
        ed.export_as();
        ed.export_tour();
        ed.export_tour_file();
        assert_eq!(mythicshot_destinations::stubs::take(), ["save dialog: export as", "folder dialog: tour folder", "save dialog: tour file"]);
        assert!(!ed.exporting(), "nothing was exported");
    }

    /// Quit never drops an export, a tour export or a publish that is running: the app stays and
    /// says so. A recording never exported asks first; "Export and close" lets the app go once the
    /// export has ended, and closes nothing before.
    #[test]
    fn quit_never_drops_a_running_export_and_waits_for_one_it_started() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ed = video_editor_with(&tmp.path().join("a"), Default::default());
        ed.dest.output_dir = tmp.path().to_path_buf();
        ed.start_export(Some(tmp.path().join("running.gif")));
        assert!(ed.exporting());
        assert_eq!(crate::editor_ui::start_quit(&mut [&mut ed], false), crate::editor_ui::QuitStart::StillRunning);
        assert!(!ed.closed && !ed.confirm_close);

        let ctx = crate::theme::test_context();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let mut ed = video_editor_with(&tmp.path().join("b"), Default::default());
        ed.dest.output_dir = tmp.path().to_path_buf();
        assert_eq!(crate::editor_ui::start_quit(&mut [&mut ed], false), crate::editor_ui::QuitStart::Asked, "a recording never exported");
        sized(&mut ed, &ctx, size, vec![]);
        let out = sized(&mut ed, &ctx, size, vec![]);
        assert!(crate::share_ui::tests::text_rect(&out, "Export this recording?").is_some());
        let at = crate::share_ui::tests::text_rect(&out, "Export and close").unwrap().center();
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), at);
        assert!(ed.exporting(), "the export started");
        assert_eq!(crate::editor_ui::quit_progress(&mut [&mut ed]), crate::editor_ui::QuitProgress::Waiting);
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while ed.exporting() && Instant::now() < deadline {
            sized(&mut ed, &ctx, size, vec![]);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!ed.exporting(), "the export ended");
        assert!(!ed.closed, "the editor did not close on its own");
        assert_eq!(crate::editor_ui::quit_progress(&mut [&mut ed]), crate::editor_ui::QuitProgress::Exit);
    }

    /// While a guide card's text is typed, Edit's Undo, Redo, Copy and Delete are disabled, and
    /// opening the menu does not end the typing: the card stays, and the next letters reach it.
    #[test]
    fn the_edit_menu_waits_while_a_guide_card_is_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let mut ed = video_editor_with(&tmp.path().join("rec"), Default::default());
        ed.test_tour_view();
        ed.select(None);
        for _ in 0..2 {
            sized(&mut ed, &ctx, size, vec![]);
        }
        click_text(&mut ed, &ctx, "Start here");
        for _ in 0..2 {
            sized(&mut ed, &ctx, size, vec![]);
        }
        assert!(ed.card_typing_id().is_some(), "typing on the card's title");
        let find = |ed: &Editor, e: Entry| crate::menu_bar::tests::find(&crate::menu_bar::menus(&ed.menu_state()), e).enabled;
        for e in [Entry::Undo, Entry::Redo, Entry::Copy, Entry::Delete] {
            assert!(!find(&ed, e), "{e:?} while the card is typed");
        }
        crate::menu_bar::tests::choose(&mut |events| sized(&mut ed, &ctx, size, events), "Edit", "Delete");
        assert!(ed.card_typing_id().is_some(), "opening the menu did not end the typing");
        let src = ed.tour_src().unwrap();
        let title = |ed: &Editor| ed.video.as_ref().unwrap().edit.tour_doc.steps.get(&src).and_then(|s| s.card.as_ref()).map(|c| c.title.clone());
        assert_eq!(title(&ed).as_deref(), Some("Start here"), "the card is still there");
        for _ in 0..3 {
            sized(&mut ed, &ctx, size, vec![]);
        }
        sized(&mut ed, &ctx, size, vec![egui::Event::Text("!".into())]);
        sized(&mut ed, &ctx, size, vec![]);
        assert!(title(&ed).is_some_and(|t| t.contains('!')), "the next letter reached the card: {:?}", title(&ed));
    }

    #[test]
    fn export_in_the_file_menu_opens_the_export_panel() {
        let tmp = tempfile::tempdir().unwrap();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        for tour in [false, true] {
            let ctx = crate::theme::test_context();
            let mut ed = video_editor_with(&tmp.path().join(format!("rec{tour}")), Default::default());
            if tour {
                ed.set_view(VideoView::Tour);
            }
            let closed = sized(&mut ed, &ctx, size, vec![]);
            assert!(crate::share_ui::tests::text_rect(&closed, "Export as…").is_none());
            crate::menu_bar::tests::choose(&mut |events| sized(&mut ed, &ctx, size, events), "File", "Export…");
            sized(&mut ed, &ctx, size, vec![]);
            let open = sized(&mut ed, &ctx, size, vec![]);
            assert!(crate::share_ui::tests::text_rect(&open, "Export as…").is_some() && crate::share_ui::tests::text_rect(&open, "Publish tour…").is_some(), "the export panel is open, tour {tour}");
            assert!(ed.doc.items().is_empty() && !ed.exporting(), "nothing was exported by it");
        }
    }

    #[test]
    fn the_properties_panel_opens_with_the_tour_view_and_closing_or_opening_it_is_remembered() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let mut ed = video_editor_with(&tmp.path().join("a"), Default::default());
        assert!(ed.prefs.tour_properties_open, "open by default");
        ed.set_view(VideoView::Tour);
        assert!(ed.properties_open());
        sized(&mut ed, &ctx, size, vec![]);
        let out = sized(&mut ed, &ctx, size, vec![]);
        let panel = properties_panel(&out).expect("the panel is open, docked at the right");
        assert!((panel.right() - size.x).abs() <= 1.0, "docked at the right edge: {panel:?}");
        // The panel sits beside the picture, never over it: the canvas ends where the panel starts.
        let canvas = crate::theme::checks::fills(&out).into_iter().filter(|(r, c, _)| *c == crate::theme::GROUND && r.height() > 200.0 && r.width() < size.x - 10.0).map(|(r, ..)| r).max_by(|a, b| a.area().total_cmp(&b.area())).expect("the canvas");
        assert!(canvas.right() <= panel.left() + 1.0, "{canvas:?} and {panel:?}");
        assert!(text_in(&out, crate::editor_ui::PROPERTIES, panel).is_some(), "the panel's heading");
        // The toolbar's button is lit: the selection fill behind its label.
        let label = crate::share_ui::tests::text_rect(&out, crate::editor_ui::PROPERTIES).unwrap();
        assert!(label.right() > size.x - 120.0 && label.top() < 80.0, "the toolbar's button comes first, at the right: {label:?}");
        assert!(crate::theme::checks::fills(&out).iter().any(|(r, c, _)| *c == crate::theme::SELECTION && r.contains(label.center()) && r.area() < 20_000.0), "the button is lit while the panel is open");

        // Closed with the toolbar's button: gone, and remembered.
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), label.center());
        assert!(!ed.properties_open() && !ed.prefs.tour_properties_open && ed.prefs_changed);
        let out = sized(&mut ed, &ctx, size, vec![]);
        assert!(properties_panel(&out).is_none(), "the panel is closed");
        assert!(!crate::theme::checks::fills(&out).iter().any(|(r, c, _)| *c == crate::theme::SELECTION && r.contains(label.center()) && r.area() < 20_000.0), "and the button is not lit");
        // Back to the Tour view later: still closed.
        ed.set_view(VideoView::Timeline);
        ed.set_view(VideoView::Tour);
        assert!(!ed.properties_open());
        // After a restart: the preference went through the settings file, and the next recording's Tour view opens without the panel.
        let restarted = |prefs: &crate::settings::EditorPrefs| {
            let mut settings = Settings::default();
            settings.editor = prefs.clone();
            let back: Settings = toml::from_str(&toml::to_string_pretty(&settings).unwrap()).unwrap();
            back.editor
        };
        let mut next = video_editor_with(&tmp.path().join("b"), restarted(&ed.prefs));
        next.set_view(VideoView::Tour);
        assert!(!next.properties_open(), "closed after a restart");

        // Opened from the Edit menu: open, and remembered as open.
        ed.prefs_changed = false;
        crate::menu_bar::tests::choose(&mut |events| sized(&mut ed, &ctx, size, events), "Edit", "Properties");
        assert!(ed.properties_open() && ed.prefs.tour_properties_open && ed.prefs_changed);
        assert!(properties_panel(&sized(&mut ed, &ctx, size, vec![])).is_some());
        let mut next = video_editor_with(&tmp.path().join("c"), restarted(&ed.prefs));
        next.set_view(VideoView::Tour);
        assert!(next.properties_open(), "open after a restart");

        // Closed with the panel's own close button, at the right of its heading: remembered too.
        let out = sized(&mut ed, &ctx, size, vec![]);
        let panel = properties_panel(&out).unwrap();
        let heading = text_in(&out, crate::editor_ui::PROPERTIES, panel).unwrap();
        let close = egui::pos2(panel.right() - 12.0 - 11.0, heading.center().y);
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), close);
        assert!(!ed.properties_open() && !ed.prefs.tour_properties_open);
        // And with the menu again, then closed again with the menu.
        crate::menu_bar::tests::choose(&mut |events| sized(&mut ed, &ctx, size, events), "Edit", "Properties");
        assert!(ed.properties_open());
        crate::menu_bar::tests::choose(&mut |events| sized(&mut ed, &ctx, size, events), "Edit", "Properties");
        assert!(!ed.properties_open() && !ed.prefs.tour_properties_open);
    }

    #[test]
    fn closing_properties_with_its_button_ends_placing_the_pulse() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ed = tour_editor(tmp.path());
        let ctx = crate::theme::test_context();
        click_text(&mut ed, &ctx, "A spot I pick");
        assert!(ed.placing_pulse().is_some());
        ed.toggle_properties();
        tour_frame(&mut ed, &ctx, vec![]);
        assert_eq!(ed.placing_pulse(), None, "the Pulse row went with the panel");
        ed.toggle_properties();
        tour_frame(&mut ed, &ctx, vec![]);
        assert_eq!(ed.placing_pulse(), None, "and does not take it again by itself");
    }

    #[test]
    fn the_properties_panel_shows_this_step_first_then_the_tour() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        // Tall enough that the whole panel is in sight.
        let size = egui::vec2(1280.0, 2400.0);
        let mut ed = video_editor_with(&tmp.path().join("rec"), Default::default());
        ed.test_tour_view();
        sized(&mut ed, &ctx, size, vec![]);
        let out = sized(&mut ed, &ctx, size, vec![]);
        let panel = properties_panel(&out).expect("open");
        let order = ["Properties", "This step", "Step 1", "Pulse", "Zoom to the spotlight", "Continue on a click anywhere", "Tour", "How the tour looks and plays. None of it is drawn into the step images.", "Card style", "Accent colour", "Zoom", "Spotlight", "Hotspot", "Backdrop", "Progress bar", "Cover card", "Always-on button", "End card", "Autoplay", "In this editor", "Animate hotspot"];
        let ys: Vec<f32> = order.iter().map(|t| text_in(&out, t, panel).unwrap_or_else(|| panic!("the panel shows {t}")).top()).collect();
        for (w, t) in ys.windows(2).zip(order.windows(2)) {
            assert!(w[0] < w[1], "{} comes before {}: {ys:?}", t[0], t[1]);
        }
        // The two sections' titles are a size up from the headings inside them.
        let height = |t: &str| text_in(&out, t, panel).unwrap().height();
        assert!(height("This step") > height("Card style") && height("Tour") > height("Card style"));
        assert!((height("This step") - height("Tour")).abs() < 0.5);
        // The old name is gone from the window.
        assert!(crate::share_ui::tests::text_rect(&out, "Tour settings").is_none());
    }

    #[test]
    fn the_tour_strip_has_the_switch_and_the_step_count_at_the_left_and_the_steps_actions_at_the_right() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = crate::theme::test_context();
        let size = crate::editor_ui::dialog_checks::WINDOW.size();
        let mut ed = video_editor_with(&tmp.path().join("rec"), Default::default());
        ed.set_view(VideoView::Tour);
        sized(&mut ed, &ctx, size, vec![]);
        let out = sized(&mut ed, &ctx, size, vec![]);
        let count = crate::tour_view::planned_steps(ed.video.as_ref().unwrap()).unwrap().len();
        let step = format!("Step 1 of {count}");
        let row: Vec<egui::Rect> = ["Timeline", "Tour", step.as_str(), "Copy to all steps", "Remove step"].iter().map(|t| {
            crate::share_ui::tests::shapes_of(&out).into_iter().rev().find_map(|s| match s {
                egui::Shape::Text(x) if x.galley.text() == *t && x.pos.y > size.y / 2.0 => Some(x.galley.rect.translate(x.pos.to_vec2())),
                _ => None,
            }).unwrap_or_else(|| panic!("the strip shows {t}"))
        }).collect();
        assert!(row.windows(2).all(|w| w[0].right() < w[1].left() && (w[0].center().y - w[1].center().y).abs() < 2.0), "one row, in this order: {row:?}");
        assert!(row[0].left() < 40.0, "the switch at the left: {row:?}");
        assert!(row[4].right() > size.x - 30.0, "Remove step at the right end: {row:?}");
        assert!(row[3].left() - row[2].right() > 200.0, "the step's actions apart from the count: {row:?}");
        assert!(crate::share_ui::tests::text_rect(&out, "Tour settings").is_none(), "no Tour settings button");
        // One switch: the picked part filled in the primary colour, the other not, joined edge to edge.
        let fills = crate::theme::checks::fills(&out);
        let part = |r: egui::Rect| fills.iter().filter(|(f, ..)| f.contains(r.center()) && f.area() < 5_000.0).min_by(|a, b| a.0.area().total_cmp(&b.0.area())).copied().unwrap();
        let (timeline, tour) = (part(row[0]), part(row[1]));
        assert_eq!((tour.1, timeline.1 == crate::theme::PRIMARY), (crate::theme::PRIMARY, false), "{timeline:?} {tour:?}");
        assert!((timeline.0.right() - tour.0.left()).abs() < 0.5 && (timeline.0.height() - tour.0.height()).abs() < 0.5, "joined: {timeline:?} {tour:?}");
        // Its parts switch the view, as the two buttons did.
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), row[0].center());
        assert!(!ed.in_tour_view());
        let out = sized(&mut ed, &ctx, size, vec![]);
        let timeline_now = crate::share_ui::tests::shapes_of(&out).into_iter().rev().find_map(|s| match s {
            egui::Shape::Text(x) if x.galley.text() == "Timeline" && x.pos.y > size.y / 2.0 => Some(x.galley.rect.translate(x.pos.to_vec2())),
            _ => None,
        }).unwrap();
        assert!(crate::theme::checks::fills(&out).iter().any(|(f, c, _)| *c == crate::theme::PRIMARY && f.contains(timeline_now.center()) && f.area() < 5_000.0), "Timeline is the filled part now");
        let tour_now = crate::share_ui::tests::shapes_of(&out).into_iter().rev().find_map(|s| match s {
            egui::Shape::Text(x) if x.galley.text() == "Tour" && x.pos.y > size.y / 2.0 => Some(x.galley.rect.translate(x.pos.to_vec2())),
            _ => None,
        }).unwrap();
        crate::menu_bar::tests::click_at(&mut |events| sized(&mut ed, &ctx, size, events), tour_now.center());
        assert!(ed.in_tour_view());
        // The same hover texts as the buttons had.
        assert_eq!(crate::tour_view::VIEW_PARTS.map(|(_, label, tip)| (label, tip)), [("Timeline", "Trim, time and play the recording"), ("Tour", "Give each tour step text, bubbles and a spotlight")]);
    }

    /// Nothing sticks out of the toolbar or the Tour view's strip at the narrowest window the
    /// editor allows, at 100%, 125% and 200%: the Timeline view, and the Tour view with the panel
    /// open, with a guide card selected (the card's controls in the toolbar) and with a removed
    /// step to restore (the strip's longest row). Nothing of the window lets the desktop through.
    #[test]
    fn nothing_sticks_out_of_the_toolbar_or_the_strip_at_the_narrowest_window_and_every_scale() {
        let tmp = tempfile::tempdir().unwrap();
        let size = egui::vec2(crate::editor_ui::EDITOR_MIN_SIZE[0], crate::editor_ui::EDITOR_MIN_SIZE[1]);
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        for ppp in [1.0, 1.25, 2.0] {
            for case in ["timeline", "timeline-open", "tour", "card", "removed"] {
                let ctx = crate::theme::test_context();
                ctx.set_pixels_per_point(ppp);
                let prefs = crate::settings::EditorPrefs { timeline_properties_open: case == "timeline-open", ..Default::default() };
                let mut ed = video_editor_with(&tmp.path().join(format!("{case}{ppp}")), prefs);
                if !case.starts_with("timeline") {
                    ed.test_tour_view();
                }
                if case == "card" {
                    ed.select(None);
                    ed.select_card(true);
                }
                if case == "removed" {
                    let steps = crate::tour_view::planned_steps(ed.video.as_ref().unwrap()).unwrap();
                    let last = steps.last().unwrap().src;
                    ed.video.as_mut().unwrap().edit.tour_removed.insert(last);
                }
                for _ in 0..2 {
                    sized(&mut ed, &ctx, size, vec![]);
                }
                let out = sized(&mut ed, &ctx, size, vec![]);
                let what = format!("{case} at {ppp}");
                crate::editor_ui::tests::assert_toolbar_fits(&out, size, &what);
                assert_eq!(properties_panel(&out).is_some(), case != "timeline", "{what}: the panel");
                if !case.starts_with("timeline") {
                    // The strip's first row: every text inside the window, none on top of another.
                    let row: Vec<(String, egui::Rect)> = crate::share_ui::tests::shapes_of(&out).into_iter().filter_map(|s| match s {
                        egui::Shape::Text(x) => Some((x.galley.text().to_string(), x.visual_bounding_rect())),
                        _ => None,
                    }).filter(|(t, r)| r.center().y > size.y * 0.5 && (["Timeline", "Tour", "Copy to all steps", "Remove step"].contains(&t.as_str()) || t.starts_with("Step ") || t.starts_with("Restore "))).collect();
                    assert!(row.len() >= 5, "{what}: {row:?}");
                    for (i, (t, r)) in row.iter().enumerate() {
                        assert!(window.contains_rect(*r), "{what}: {t:?} at {r:?} leaves the window");
                        for (u, q) in &row[i + 1..] {
                            assert!(!r.intersects(*q), "{what}: {t:?} at {r:?} is on top of {u:?} at {q:?}");
                        }
                    }
                }
                assert!(crate::theme::checks::assert_texts_fit(&out, &what) >= 5);
                assert_eq!(crate::snapshot::uncovered(&crate::theme::test_context(), size, ppp, 3, |ui| ed.ui(ui)), 0, "{what}");
            }
        }
    }

    /// Pictures for the owner, drawn without a window (see `snapshot`): the Tour view with the
    /// Properties panel open, the same at the narrowest window, and the File and Edit menus open.
    #[test]
    #[ignore = "writes pictures: MYTHICSHOT_SNAPSHOTS=<folder>"]
    fn snapshot_the_menus_and_the_properties_panel() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, size) in [("tour-properties", crate::editor_ui::dialog_checks::WINDOW.size()), ("tour-properties-narrow", egui::vec2(crate::editor_ui::EDITOR_MIN_SIZE[0], crate::editor_ui::EDITOR_MIN_SIZE[1]))] {
            let mut ed = video_editor_with(&tmp.path().join(name), Default::default());
            ed.test_tour_view();
            let shot = crate::snapshot::shot(&crate::theme::test_context(), size, 1.5, 4, |ui| ed.ui(ui));
            assert_eq!(crate::snapshot::save(&shot, name), 0, "{name}");
        }
        for menu in ["File", "Edit"] {
            let ctx = crate::theme::test_context();
            let size = crate::editor_ui::dialog_checks::WINDOW.size();
            let mut ed = video_editor_with(&tmp.path().join(menu), Default::default());
            ed.test_tour_view();
            crate::menu_bar::tests::open_menu(&mut |events| sized(&mut ed, &ctx, size, events), menu);
            let shot = crate::snapshot::shot(&ctx, size, 1.5, 2, |ui| ed.ui(ui));
            let name = format!("menu-{}", menu.to_lowercase());
            assert_eq!(crate::snapshot::save(&shot, &name), 0, "{name}");
        }
    }

    // ---- Moving a step's pulse in the Tour view ----

    /// One frame of `ed` with `events`, a second after the last, in the editor's own window size.
    fn tour_frame(ed: &mut Editor, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        let input = egui::RawInput { time: Some(ctx.input(|i| i.time) + 1.0), ..crate::editor_ui::dialog_checks::input(events) };
        let mut out = ctx.run_ui(input, |ui| ed.ui(ui));
        out.textures_delta.clear();
        out
    }

    /// A press and release on the text `label`, as a user clicks it; the Properties panel is
    /// scrolled to its end first when the text is not in sight.
    fn click_text(ed: &mut Editor, ctx: &egui::Context, label: &str) {
        let mut out = tour_frame(ed, ctx, vec![]);
        if crate::share_ui::tests::text_rect(&out, label).is_none() {
            let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -4000.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
            tour_frame(ed, ctx, vec![egui::Event::PointerMoved(egui::pos2(1200.0, 400.0)), wheel]);
            for _ in 0..3 {
                out = tour_frame(ed, ctx, vec![]);
            }
        }
        let at = crate::share_ui::tests::text_rect(&out, label).unwrap_or_else(|| panic!("{label} is drawn")).center();
        crate::editor_ui::dialog_checks::click(ed, ctx, at);
    }

    /// Where the hotspot ring is drawn on the screen, if it is.
    fn hotspot_on_screen(out: &egui::FullOutput) -> Option<egui::Pos2> {
        crate::share_ui::tests::shapes_of(out).into_iter().find_map(|s| match s {
            egui::Shape::Circle(c) if (c.radius - 15.0).abs() < 0.01 && c.stroke.width > 0.0 => Some(c.center),
            _ => None,
        })
    }

    /// A drag with the primary button from `from` to `to`, over a few frames.
    fn drag(ed: &mut Editor, ctx: &egui::Context, from: egui::Pos2, to: egui::Pos2) {
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        tour_frame(ed, ctx, vec![egui::Event::PointerMoved(from)]);
        tour_frame(ed, ctx, vec![button(from, true)]);
        for i in 1..=4 {
            tour_frame(ed, ctx, vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0))]);
        }
        tour_frame(ed, ctx, vec![button(to, false)]);
        tour_frame(ed, ctx, vec![]);
    }

    fn ctrl(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND }
    }

    fn plain_key(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    /// The shown step's moved pulse, and its click as the tour has it.
    fn shown_pulse(ed: &Editor) -> (Option<crate::tour::Point>, Option<(f32, f32)>) {
        let v = ed.video.as_ref().unwrap();
        let steps = crate::tour_view::planned_steps(v).unwrap();
        let step = steps.iter().find(|s| s.src == v.current).unwrap();
        (v.edit.tour_doc.steps.get(&step.src).and_then(|s| s.pulse.clone()), step.click)
    }

    /// An editor of `state(root)` with its edit (the crop, trim and removed step) and no annotations, in the Tour view.
    fn tour_editor(root: &Path) -> Editor {
        let mut st = state(root);
        st.items.clear();
        let mut ed = editor_of(&st);
        ed.video.as_mut().unwrap().edit = st.edit.clone();
        ed.init_pulse_history();
        ed.set_view(VideoView::Tour);
        ed.video.as_mut().unwrap().tour.settings.open = true;
        ed
    }

    #[test]
    fn the_pulse_is_dragged_on_the_picture_undone_in_one_step_and_put_back() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ed = tour_editor(tmp.path());
        ed.keys = crate::hotkeys::EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect();
        ed.use_select_tool();
        let ctx = crate::theme::test_context();
        let out = tour_frame(&mut ed, &ctx, vec![]);
        let (none, recorded) = shown_pulse(&ed);
        assert_eq!(none, None);
        let ring = hotspot_on_screen(&out).expect("the hotspot is drawn where the step was clicked");
        // Nothing moves the pulse until "A spot I pick" is chosen: a drag on it is a drag of the tools.
        drag(&mut ed, &ctx, ring, ring + egui::vec2(30.0, 20.0));
        assert_eq!(shown_pulse(&ed).0, None);
        assert!(crate::share_ui::tests::text_rect(&tour_frame(&mut ed, &ctx, vec![]), crate::tour_view::PULSE_HINT).is_none());
        assert!(!ed.doc.can_undo(), "a drag on the empty picture with the Select tool changes nothing");

        click_text(&mut ed, &ctx, "A spot I pick");
        let src = ed.tour_src().unwrap();
        assert_eq!(ed.placing_pulse(), Some(src));
        let out = tour_frame(&mut ed, &ctx, vec![]);
        assert!(crate::share_ui::tests::text_rect(&out, "Drag the pulse on the picture").is_some(), "the hint shows");
        assert!(crate::share_ui::tests::text_rect(&out, "Put it back where I clicked").is_none(), "nothing to put back yet");
        assert!(!ed.doc.can_undo(), "choosing it changes nothing yet");

        // A drag of the pulse moves it, and the tour's click with it.
        drag(&mut ed, &ctx, ring, ring + egui::vec2(30.0, 20.0));
        let (moved, click) = shown_pulse(&ed);
        assert!(moved.is_some());
        assert_ne!(click, recorded, "the step's click is where the pulse went");
        let out = tour_frame(&mut ed, &ctx, vec![]);
        let at = hotspot_on_screen(&out).unwrap();
        assert!((at - (ring + egui::vec2(30.0, 20.0))).length() < 1.5, "drawn where it was dropped: {at:?} from {ring:?}");
        assert!(crate::share_ui::tests::text_rect(&out, "Put it back where I clicked").is_some());

        // The whole drag is one undo step, and redo brings it back.
        tour_frame(&mut ed, &ctx, vec![ctrl(egui::Key::Z)]);
        assert_eq!(shown_pulse(&ed), (None, recorded));
        assert!(!ed.doc.can_undo(), "one step for the drag");
        assert!((hotspot_on_screen(&tour_frame(&mut ed, &ctx, vec![])).unwrap() - ring).length() < 0.5);
        tour_frame(&mut ed, &ctx, vec![ctrl(egui::Key::Y)]);
        assert_eq!(shown_pulse(&ed), (moved.clone(), click));

        // A click on the picture puts it there too: one more undo step.
        crate::editor_ui::dialog_checks::click(&mut ed, &ctx, ring + egui::vec2(-20.0, 10.0));
        let (clicked, _) = shown_pulse(&ed);
        assert!(clicked.is_some() && clicked != moved);
        tour_frame(&mut ed, &ctx, vec![ctrl(egui::Key::Z)]);
        assert_eq!(shown_pulse(&ed).0, moved);

        // "Put it back where I clicked" returns to the recorded click, and is undone like any change.
        click_text(&mut ed, &ctx, "Put it back where I clicked");
        assert_eq!(shown_pulse(&ed), (None, recorded));
        tour_frame(&mut ed, &ctx, vec![ctrl(egui::Key::Z)]);
        assert_eq!(shown_pulse(&ed), (moved.clone(), click));

        // Esc stops the picture taking the pulse; the moved pulse stays.
        assert_eq!(ed.placing_pulse(), Some(src));
        tour_frame(&mut ed, &ctx, vec![plain_key(egui::Key::Escape)]);
        assert_eq!(ed.placing_pulse(), None);
        assert_eq!(shown_pulse(&ed).0, moved);
        assert!(crate::share_ui::tests::text_rect(&tour_frame(&mut ed, &ctx, vec![]), crate::tour_view::PULSE_HINT).is_none());

        // "Where I clicked" puts it back too, and stops placing.
        click_text(&mut ed, &ctx, "A spot I pick");
        click_text(&mut ed, &ctx, "Where I clicked");
        assert_eq!((shown_pulse(&ed), ed.placing_pulse()), ((None, recorded), None));
    }

    #[test]
    fn the_pulse_stays_inside_the_kept_picture_and_the_end_step_has_none_to_move() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ed = tour_editor(tmp.path());
        let ctx = crate::theme::test_context();
        click_text(&mut ed, &ctx, "A spot I pick");
        let ring = hotspot_on_screen(&tour_frame(&mut ed, &ctx, vec![])).unwrap();
        // Dragged far past the crop's corner: it stops at the crop's last pixel, so the tour keeps it.
        drag(&mut ed, &ctx, ring, ring + egui::vec2(-2000.0, -2000.0));
        assert_eq!(shown_pulse(&ed).0, Some(crate::tour::Point { x: 20.0, y: 10.0 }), "the crop starts at 20, 10");
        assert_eq!(shown_pulse(&ed).1, Some((0.0, 0.0)));
        let now = hotspot_on_screen(&tour_frame(&mut ed, &ctx, vec![])).unwrap();
        drag(&mut ed, &ctx, now, now + egui::vec2(2000.0, 2000.0));
        assert_eq!(shown_pulse(&ed).0, Some(crate::tour::Point { x: 179.0, y: 149.0 }), "the crop ends before 180, 150");
        // Closing Tour settings stops the picture taking the pulse.
        ed.video.as_mut().unwrap().tour.settings.open = false;
        tour_frame(&mut ed, &ctx, vec![]);
        assert_eq!(ed.placing_pulse(), None);
        ed.video.as_mut().unwrap().tour.settings.open = true;
        tour_frame(&mut ed, &ctx, vec![]);
        assert_eq!(ed.placing_pulse(), None, "and does not take it again by itself");
        // The end step has no click: its row is there but does nothing.
        let last = crate::tour_view::planned_steps(ed.video.as_ref().unwrap()).unwrap().len() - 1;
        for _ in 0..last {
            tour_frame(&mut ed, &ctx, vec![plain_key(egui::Key::ArrowRight)]);
        }
        assert_eq!(shown_pulse(&ed).1, None, "the end step");
        click_text(&mut ed, &ctx, "A spot I pick");
        assert_eq!(ed.placing_pulse(), None);
        assert!(crate::share_ui::tests::text_rect(&tour_frame(&mut ed, &ctx, vec![]), crate::tour_view::PULSE_HINT).is_none());
    }

    #[test]
    fn a_moved_pulse_is_saved_with_the_project_and_reopens_with_undo_starting_there() {
        let tmp = tempfile::tempdir().unwrap();
        let mut st = state(tmp.path());
        let steps = tour::plan_steps(&st.rec, &st.edit, &st.events).unwrap();
        st.edit.tour_doc.steps.entry(steps[0].src).or_default().pulse = Some(crate::tour::Point { x: 100.0, y: 100.0 });
        let store = store_in(tmp.path());
        let key = ProjectKey { server: "https://tours.example.com".into(), tour_id: TOUR.into(), chapter_id: CH.into() };
        let snap = Snapshot { profile: "", rec: &st.rec, title: TITLE, tour_title: TOUR_TITLE, steps: 2, edit: &st.edit, events: &st.events, items: &st.items };
        store.save(&key, &snap).unwrap();
        let mut ed = reopened(&store, &key);
        let at = |ed: &Editor| ed.video.as_ref().unwrap().edit.tour_doc.steps.get(&steps[0].src).and_then(|s| s.pulse.clone());
        assert_eq!(at(&ed), Some(crate::tour::Point { x: 100.0, y: 100.0 }));
        // Put back and undone: the saved spot, not the recorded click, is where undo goes back to.
        assert!(ed.set_pulse(steps[0].src, None, true));
        assert_eq!(at(&ed), None);
        ed.doc.undo();
        ed.pulses_from_history();
        assert_eq!(at(&ed), Some(crate::tour::Point { x: 100.0, y: 100.0 }));
    }

    fn reopened(store: &Store, key: &ProjectKey) -> Editor {
        let s = Settings::default();
        Editor::open_project(store.load(key).unwrap(), Default::default(), s.dest_context(TITLE), OverlayStyle::default(), s.editor.clone()).unwrap()
    }

    /// Ask the editor's window to close, as the window's own close button does.
    fn request_close(ed: &mut Editor) {
        let ctx = crate::theme::test_context();
        let mut input = egui::RawInput::default();
        input.viewports.insert(egui::ViewportId::ROOT, egui::ViewportInfo { events: vec![egui::ViewportEvent::Close], ..Default::default() });
        ctx.run_ui(input, |ui| ed.ui(ui)).textures_delta.clear();
    }

    #[test]
    fn a_published_tour_leaves_a_project_tied_to_its_address_tour_and_chapter() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let keep = keeping(true, Some(store.root().to_path_buf()));
        assert!(matches!(&keep, Keeping::In(at) if at.root() == store.root()));
        let done = publish_to(&s, &state, true, &keep).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }), "{done:?}");
        assert_eq!((done.finished.tour_id.as_str(), done.finished.chapter_id.as_str()), (TOUR, CH));
        assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: None });
        // One project, for exactly the address the upload went to, the tour and the chapter.
        let key = key_at(&s);
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!((&list[0].key, list[0].title.as_str(), list[0].tour_title.as_str(), list[0].problem), (&key, TITLE, TOUR_TITLE, None));
        assert_eq!(store.for_tour(&key.server, TOUR).len(), 1);
        assert!(store.for_tour("https://other.example.com", TOUR).is_empty());
        // It holds the clicks and no key press, and never the token.
        let text = project_text(&list[0]);
        assert!(!text.contains(TOKEN) && !text.contains("Ctrl"), "{text}");
        let loaded = store.load(&key).unwrap();
        assert_eq!(loaded.restored.events, [left(750, 40, 30), left(1250, 60, 50), left(2250, 120, 90)]);
    }

    #[test]
    fn a_draft_upload_leaves_a_project_too() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let done = publish_to(&s, &state, false, &Keeping::In(store.clone())).unwrap();
        assert_eq!((done.finished.outcome.clone(), done.kept.clone()), (Outcome::Draft, Kept::Saved { left_out: vec![], over: None }));
        assert!(!s.lines().iter().any(|l| l == PUBLISH));
        assert_eq!(store.list().iter().map(|p| p.key.clone()).collect::<Vec<_>>(), [key_at(&s)]);
    }

    #[test]
    fn the_project_is_saved_before_the_publish_step_so_a_failed_publish_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        // The site's publish route looks at the store each time it is called: first it fails.
        let seen_at_publish = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (looking, log) = (store.clone(), seen_at_publish.clone());
        let s = publish_stub(move |calls| {
            log.lock().unwrap().push(looking.list().len());
            if calls == 0 { reply(500, r#"{"error":"publish_failed","detail":"the tour could not be published"}"#) } else { reply(200, LIVE) }
        });
        let done = publish_to(&s, &state, true, &Keeping::In(store.clone())).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::NotPublic { cancelled: false, .. }), "{done:?}");
        assert_eq!(*seen_at_publish.lock().unwrap(), [1], "the project was there when the publish step began");
        assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: None });
        let key = key_at(&s);
        let saved = project_text(&store.list()[0]);
        drop(store.load(&key).unwrap());

        // "Try again" publishes the same upload: it names the chapter the commit answered, and saves nothing again.
        let mut screen = Some(ResultDialog::new(TOUR_TITLE.into(), Before::new_tour(), done));
        let retry = publish_ui::take_retry(&mut screen, false).unwrap();
        assert_eq!((retry.tour_id.as_str(), retry.chapter_id.as_str(), &retry.kept), (TOUR, CH, &Kept::Saved { left_out: vec![], over: None }));
        let uploads = s.lines().iter().filter(|l| l.ends_with("/commit")).count();
        let again = retry_done(&retry, crate::publish::publish_tour(&s.base, TOKEN, &retry.tour_id, |_| true));
        assert!(matches!(again.finished.outcome, Outcome::Public { .. }), "{again:?}");
        assert_eq!((again.finished.tour_id.as_str(), again.finished.chapter_id.as_str(), &again.kept), (TOUR, CH, &Kept::Saved { left_out: vec![], over: None }));
        assert_eq!(s.lines().iter().filter(|l| l.ends_with("/commit")).count(), uploads, "nothing was uploaded again");
        assert_eq!(store.list().len(), 1);
        assert_eq!(project_text(&store.list()[0]), saved, "the project was not written a second time");
    }

    #[test]
    fn an_upload_that_fails_or_is_cancelled_saves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let keep = Keeping::In(store.clone());
        // The commit fails.
        let s = stub(|req, seen| if req.line.ends_with("/commit") { reply(500, r#"{"error":"commit_failed","detail":"no"}"#) } else { platform(req, seen) });
        assert!(publish_to(&s, &state, true, &keep).is_err());
        // Cancelled while the pictures go up, and cancelled before anything is sent.
        let s = publish_stub(|_| reply(200, LIVE));
        let run = Arc::new(Run::default());
        let sent = std::cell::Cell::new(false);
        let cancelled = publish_job(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &keep, &run, &|f| {
            if f > PUBLISH_WRITE_SHARE {
                sent.set(true);
                run.cancel();
            }
        });
        assert_eq!(cancelled, Err(CANCELLED.to_string()));
        assert!(sent.get() && !s.lines().iter().any(|l| l.ends_with("/commit")), "{:?}", s.lines());
        let before = s.lines().len();
        let run = Arc::new(Run::default());
        run.cancel();
        assert_eq!(publish_job(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &keep, &run, &|_| {}), Err(CANCELLED.to_string()));
        // No token: nothing is sent at all.
        let no_token = publish_job(&state, TITLE, TOUR_TITLE, &s.base, || Err("Add the server and token first.".to_string()), &Target::New, true, &keep, &Arc::new(Run::default()), &|_| {});
        assert_eq!(no_token, Err("Add the server and token first.".to_string()));
        assert_eq!(s.lines().len(), before);
        // Not one project, and not even the folder for them.
        assert!(store.list().is_empty());
        assert!(!store.root().exists());
    }

    #[test]
    fn with_the_switch_off_nothing_is_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let keep = keeping(false, Some(store.root().to_path_buf()));
        assert!(matches!(keep, Keeping::Off));
        let done = publish_to(&s, &state, true, &keep).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }));
        assert_eq!(done.kept, Kept::Off);
        assert_eq!(publish_ui::kept_note(&done.kept, false), None);
        assert!(!store.root().exists());
        // Off stays off without a folder too.
        assert!(matches!(keeping(false, None), Keeping::Off));
    }

    #[test]
    fn a_project_that_cannot_be_saved_does_not_fail_the_publish_and_the_result_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        // Where the folder for saved recordings should be, there is a file.
        let store = store_in(tmp.path());
        std::fs::write(store.root(), b"in the way").unwrap();
        let done = publish_to(&s, &state, true, &Keeping::In(store.clone())).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }), "the tour is published all the same: {done:?}");
        let Kept::Failed(reason) = &done.kept else { panic!("{:?}", done.kept) };
        assert_eq!(reason, "the folder for saved recordings could not be made.");
        let note = publish_ui::kept_note(&done.kept, false).unwrap();
        assert_eq!(note, "The recording could not be saved on this computer, so it can't be edited later: the folder for saved recordings could not be made.");
        assert_eq!(std::fs::read(store.root()).unwrap(), b"in the way");
        // No folder for saved recordings on this computer: keeping is off, and the result says so once.
        let keep = keeping(true, None);
        assert!(matches!(keep, Keeping::NoFolder));
        let s = publish_stub(|_| reply(200, LIVE));
        let done = publish_to(&s, &state, false, &keep).unwrap();
        assert_eq!(done.finished.outcome, Outcome::Draft);
        assert_eq!(done.kept, Kept::Failed(publish_ui::NO_FOLDER_FOR_RECORDINGS.into()));
    }

    #[test]
    fn closing_the_editor_after_publishing_deletes_the_temporary_recording_and_keeps_the_saved_one() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let temporary = tmp.path().join("rec");
        let third = state.rec.frame(2).unwrap().into_raw();
        let ed = editor_of(&state);
        assert_eq!(publish_to(&s, &state, true, &Keeping::In(store.clone())).unwrap().kept, Kept::Saved { left_out: vec![], over: None });
        assert!(temporary.join("frames.zst").exists());
        // The job has ended and the editor closes: the recording's own folder goes.
        let frames = state.rec.frames.len();
        drop(state);
        drop(ed);
        assert!(!temporary.exists(), "the temporary recording is deleted");
        // The saved one is whole: it opens, and every frame decodes.
        let loaded = store.load(&key_at(&s)).unwrap();
        assert_eq!(loaded.rec.frames.len(), frames);
        let mut reader = FrameReader::new(loaded.rec.clone()).unwrap();
        for i in 0..frames {
            reader.get(i).unwrap();
        }
        assert_eq!(loaded.rec.frame(2).unwrap().into_raw(), third);
    }

    #[test]
    fn a_recording_closed_without_uploading_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let ed = editor_of(&state);
        // An upload the site refused changes nothing about that.
        let s = stub(|_, _| reply(401, r#"{"error":"unauthorized"}"#));
        assert!(publish_to(&s, &state, true, &Keeping::In(store.clone())).is_err());
        drop(state);
        drop(ed);
        assert!(!tmp.path().join("rec").exists());
        assert!(!store.root().exists());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "nothing is left of the recording");
    }

    #[test]
    fn a_reopened_project_shows_the_saved_trim_crop_removed_steps_cards_and_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        publish_to(&s, &state, true, &Keeping::In(store.clone())).unwrap();
        let key = key_at(&s);
        let was = tour::plan_steps(&state.rec, &state.edit, &state.events).unwrap();

        // Opened with another overlay default than it was saved with: the saved one shows.
        let other = OverlayStyle { keys: KeyStyle::Keyboard, ..OverlayStyle::default() };
        let settings = Settings::default();
        let mut ed = Editor::open_project(store.load(&key).unwrap(), Default::default(), settings.dest_context(TITLE), other, settings.editor.clone()).unwrap();
        assert_eq!(ed.title(), TITLE);
        assert_eq!(ed.status_text(), None, "nothing was left out, so nothing is said");
        let v = ed.video.as_ref().unwrap();
        let (e, saved) = (&v.edit, &state.edit);
        assert_eq!((e.trim, e.crop, &e.deleted, &e.tour_removed, e.scale, e.reverse), (saved.trim, saved.crop, &saved.deleted, &saved.tour_removed, saved.scale, saved.reverse));
        assert_eq!(e.tour_doc, saved.tour_doc, "the cover and the guide card");
        assert_eq!((e.overlays, v.last_overlays), (saved.overlays, saved.overlays));
        assert_eq!(v.current, saved.trim.0);
        assert_eq!(tour::plan_steps(&v.rec, &v.edit, &v.events).unwrap(), was, "the same steps, with the removed one still removed");
        // The clicks are back; the key press was never saved.
        assert_eq!(v.events, [left(750, 40, 30), left(1250, 60, 50), left(2250, 120, 90)]);
        // The rectangle and the note, as they were, and a new item gets the next number.
        assert_eq!(ed.doc.items(), &state.items[..]);
        assert!(!ed.doc.dirty);
        // It knows where it came from, and it counts as exported.
        let origin = v.origin.as_ref().unwrap();
        assert_eq!((&origin.key, origin.tour_title.as_str(), v.exported), (&key, TOUR_TITLE, true));
        assert_eq!(ed.add_item(Shape::Rect(Bounds::new(pt(5.0, 5.0), pt(9.0, 9.0)))), 3);

        // While it is open the project cannot be opened a second time; after it closes it can.
        assert!(matches!(store.load(&key), Err(LoadError::AlreadyOpen)));
        drop(ed);
        let mut loaded = store.load(&key).unwrap();

        // What could not be restored is said once, in the status bar.
        loaded.restored.dropped = vec!["the cover logo".into(), "a pasted picture".into(), "a pasted picture".into()];
        let ed = Editor::open_project(loaded, Default::default(), settings.dest_context(TITLE), other, settings.editor.clone()).unwrap();
        assert_eq!(ed.status_text(), Some("This recording was opened without the cover logo and 2 pasted pictures."));
        assert_eq!(opened_note(&[]), None);
        // It stays in front of what the editor reports next.
        let mut ed = ed;
        ed.notify("Exporting… 0%");
        assert_eq!(ed.status_text(), Some("This recording was opened without the cover logo and 2 pasted pictures."));
    }

    #[test]
    fn closing_a_reopened_project_does_not_ask_to_export_and_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        publish_to(&s, &state, true, &Keeping::In(store.clone())).unwrap();
        let key = key_at(&s);

        // A new recording that was never exported asks before it closes.
        let mut fresh = editor_of(&state);
        request_close(&mut fresh);
        assert!(!fresh.closed);
        drop(fresh);
        drop(state);

        let info = store.list().remove(0);
        let files = |dir: &Path| {
            let mut names: Vec<(String, u64)> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap()).map(|e| (e.file_name().to_string_lossy().into_owned(), e.metadata().unwrap().len())).collect();
            names.sort();
            names
        };
        let (text, before) = (project_text(&info), files(&info.dir));
        // Reopened and changed, and never sent to the site: closing just closes.
        let mut ed = reopened(&store, &key);
        ed.add_item(Shape::Rect(Bounds::new(pt(5.0, 5.0), pt(50.0, 40.0))));
        assert!(ed.doc.dirty);
        request_close(&mut ed);
        assert!(ed.closed, "no question: it would say the recording is deleted");
        drop(ed);
        // The saved recording is as it was, and opens again.
        assert_eq!((project_text(&info), files(&info.dir)), (text, before));
        assert_eq!(store.list(), [info]);
        let again = reopened(&store, &key);
        assert_eq!(again.doc.items().len(), 2, "the change that was not sent is gone");
    }

    /// Wait until `done` holds, for at most ten seconds.
    fn eventually(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// A publish job that waits to be told, then does `work`, as a job that is still saving.
    fn waiting_job(work: impl FnOnce() -> Kept + Send + 'static) -> (ExportJob, std::sync::mpsc::Sender<()>, Arc<Run>) {
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let run = Arc::new(Run::default());
        let job = EncodeJob::spawn(move |_| {
            let _ = wait.recv();
            let kept = work();
            Ok(PublishDone { finished: Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 2, outcome: Outcome::Draft }, kept })
        });
        (ExportJob::Publish(job, run.clone(), TOUR_TITLE.into(), Before::new_tour(), false), go, run)
    }

    #[test]
    fn a_new_recording_closed_while_its_job_still_saves_keeps_its_files_until_the_job_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let temporary = tmp.path().join("rec");
        let key = ProjectKey { server: "https://tours.example.com".into(), tour_id: TOUR.into(), chapter_id: CH.into() };
        let mut ed = editor_of(&state);
        // The job saves the recording once it is told to; until then it is "still saving".
        let (saving, to_key) = (store.clone(), key.clone());
        let (job, go, run) = waiting_job(move || {
            let snap = Snapshot { profile: "", rec: &state.rec, title: TITLE, tour_title: TOUR_TITLE, steps: 2, edit: &state.edit, events: &state.events, items: &state.items };
            match saving.save(&to_key, &snap) {
                Ok(saved) => Kept::Saved { left_out: saved.left_out, over: None },
                Err(why) => Kept::Failed(why),
            }
        });
        ed.video.as_mut().unwrap().job = Some(job);
        drop(ed);
        assert!(run.is_cancelled(), "the publish is told to stop: nobody would see its result");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(temporary.join("frames.zst").exists(), "the job still reads the recording");
        // The job ends: its save found the frames, and only then the temporary folder goes.
        go.send(()).unwrap();
        eventually("the temporary recording is deleted once the job has ended", || !temporary.exists());
        let loaded = store.load(&key).unwrap();
        loaded.rec.frame(5).unwrap();
    }

    #[test]
    fn a_reopened_project_closed_while_a_job_runs_stays_open_until_the_job_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        publish_to(&s, &state, false, &Keeping::In(store.clone())).unwrap();
        drop(state);
        let key = key_at(&s);
        let info = store.list().remove(0);

        let mut ed = reopened(&store, &key);
        let (job, go, _run) = waiting_job(|| Kept::Off);
        ed.video.as_mut().unwrap().job = Some(job);
        drop(ed);
        // The editor is gone and the job is not: the project is still registered as open.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(matches!(store.load(&key), Err(LoadError::AlreadyOpen)));
        go.send(()).unwrap();
        eventually("the project is let go of once the job has ended", || !matches!(store.load(&key), Err(LoadError::AlreadyOpen)));
        // Nothing of it was removed, and it opens again.
        assert_eq!(store.list(), [info]);
        drop(reopened(&store, &key));
    }

    #[test]
    fn a_project_whose_recording_cannot_be_read_after_all_is_let_go_of_again() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        publish_to(&s, &state, false, &Keeping::In(store.clone())).unwrap();
        let key = key_at(&s);
        // Loaded, and then the frames cannot be opened: a recording with no file behind it.
        let mut loaded = store.load(&key).unwrap();
        loaded.rec = Arc::new(Recording::timing_only(200, 160, &[0, 500], 1000));
        let settings = Settings::default();
        let opened = Editor::open_project(loaded, Default::default(), settings.dest_context(TITLE), OverlayStyle::default(), settings.editor.clone());
        assert!(opened.is_err());
        // Not left registered as open: it can be opened, and it is all there.
        let again = store.load(&key).unwrap();
        assert_eq!(again.restored.title, TITLE);
    }

    /// The platform, with a commit that presses Cancel on `run` just before it answers.
    fn cancelling_at_the_commit(run: &Arc<Run>) -> Stub {
        let pressed = run.clone();
        crate::publish::tests::publish_stub_over(
            move |req, seen| {
                if req.line.ends_with("/commit") {
                    pressed.cancel();
                }
                platform(req, seen)
            },
            |_| reply(200, LIVE),
        )
    }

    #[test]
    fn cancel_pressed_after_the_commit_keeps_the_project_and_reports_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let run = Arc::new(Run::default());
        let s = cancelling_at_the_commit(&run);
        let done = publish_job(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &Keeping::In(store.clone()), &run, &|_| {}).unwrap();
        assert_eq!(done.finished.outcome, Outcome::NotPublic { cancelled: true, error: String::new() });
        assert!(!s.lines().iter().any(|l| l == PUBLISH), "nothing was published");
        // The upload is on the site, so its recording is kept: the frames are linked, which a cancel never stops.
        assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: None });
        assert_eq!(store.list().iter().map(|p| (p.key.clone(), p.problem)).collect::<Vec<_>>(), [(key_at(&s), None)]);
        assert_eq!(publish_ui::result_status(&done.finished, &Before::new_tour(), false), "Publishing cancelled: the draft is uploaded, but not public");
        assert_eq!(publish_ui::result_note(&done.finished.outcome, &Before::new_tour(), false), Some(("Publishing was cancelled. The draft is uploaded, but it is not public.".to_string(), false)));
        assert_eq!(publish_ui::kept_note(&done.kept, false), None);
    }

    #[test]
    fn a_cancel_while_the_frames_are_copied_is_a_recording_not_kept_and_never_a_failed_upload() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let run = Arc::new(Run::default());
        let s = cancelling_at_the_commit(&run);
        // A save that has to copy: it asks the job whether to go on, as the store's copy does, and stops on a no.
        let asked = std::cell::Cell::new(0);
        let copying: &SaveFn<'_> = &|store, key, snap, keep_going| {
            asked.set(asked.get() + 1);
            if keep_going() { store.save(key, snap) } else { Err(crate::project::SAVING_CANCELLED.to_string()) }
        };
        let done = publish_job_with(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &Keeping::In(store.clone()), &run, &|_| {}, copying, &NO_LIMIT).unwrap();
        assert_eq!(asked.get(), 1);
        assert_eq!(done.finished.outcome, Outcome::NotPublic { cancelled: true, error: String::new() }, "the upload is in, and the publish was cancelled");
        assert_eq!(done.kept, Kept::Failed("saving was cancelled.".into()));
        assert_eq!(publish_ui::kept_note(&done.kept, false).as_deref(), Some("The recording could not be saved on this computer, so it can't be edited later: saving was cancelled."));
        assert!(store.list().is_empty() && !store.root().exists(), "nothing is left of the save");
    }

    #[test]
    fn a_saved_video_format_is_kept_while_ffmpeg_is_looked_for_and_gives_way_only_when_there_is_none() {
        for format in [Format::Mp4, Format::Webm] {
            assert_eq!([Found::NotYet, Found::Ok, Found::Unusable, Found::None].map(|found| usable_format(format, found)), [format, format, format, Format::Gif]);
        }
        for format in [Format::Gif, Format::Apng] {
            assert_eq!([Found::NotYet, Found::Ok, Found::Unusable, Found::None].map(|found| usable_format(format, found)), [format; 4]);
        }
    }

    #[test]
    fn a_recording_that_was_kept_wakes_the_conversion_queue_and_one_that_was_not_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        // Every publish to a site of its own: a site takes a step once.
        let publish = |keeping: &Keeping, save: &SaveFn<'_>| {
            let s = publish_stub(|_| reply(200, LIVE));
            publish_job_with(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, keeping, &Arc::new(Run::default()), &|_| {}, save, &NO_LIMIT).unwrap().kept
        };
        let nudges = crate::project::nudges_here;
        let before = nudges();
        // Not kept: the switch is off, there is no folder, the save failed, the save broke.
        assert_eq!(publish(&Keeping::Off, &store_save), Kept::Off);
        assert!(matches!(publish(&Keeping::NoFolder, &store_save), Kept::Failed(_)));
        assert!(matches!(publish(&Keeping::In(store.clone()), &|_, _, _, _| Err("the disk is full.".to_string())), Kept::Failed(_)));
        assert!(matches!(publish(&Keeping::In(store.clone()), &|_, _, _, _| panic!("the save broke")), Kept::Failed(_)));
        assert_eq!(nudges(), before);
        // Kept: once.
        assert_eq!(publish(&Keeping::In(store.clone()), &store_save), Kept::Saved { left_out: vec![], over: None });
        assert_eq!((nudges(), store.list().len()), (before + 1, 1));
    }

    #[test]
    fn a_save_that_panics_is_an_internal_error_reason_and_the_upload_stays_done() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let broken: &SaveFn<'_> = &|_, _, _, _| panic!("the save broke");
        let done = publish_job_with(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}, broken, &NO_LIMIT).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }), "the publish step ran all the same: {done:?}");
        assert_eq!(done.kept, Kept::Failed(publish_ui::KEEPING_BROKE_OFF.into()));
        assert_eq!(publish_ui::kept_note(&done.kept, false).as_deref(), Some("The recording could not be saved on this computer, so it can't be edited later: saving failed because of an internal error."));
    }

    #[test]
    fn a_publish_keeps_its_recording_where_the_setting_and_this_users_folder_say() {
        let _alone = publish_ui::tests::SWITCH.lock().unwrap_or_else(|e| e.into_inner());
        let was = publish_ui::keeps_recordings();
        // Only the folder's name is worked out here: nothing in it is read or written.
        let root = Store::default_root();
        publish_ui::set_keep_recordings(true);
        match (keeping_now(), &root) {
            (Keeping::In(store), Some(root)) => assert_eq!(store.root(), root),
            (Keeping::NoFolder, None) => {}
            _ => panic!("the switch is on, and the folder is not the user's"),
        }
        publish_ui::set_keep_recordings(false);
        assert!(matches!(keeping_now(), Keeping::Off));
        publish_ui::set_keep_recordings(was);
    }

    // ---- The notice after a publish that went past the limit ----

    const GB: u64 = 1 << 30;

    /// No limit is set: the saved recordings are never measured.
    const NO_LIMIT: Limit<'static> = Limit { warn_gb: 0, totals: &Store::totals };

    /// A publish to a site of its own, into an empty store, on a computer whose earlier
    /// recordings take `others` bytes and whose limit is `warn_gb`: what the job answers, how
    /// often it measured the saved recordings, and what the store holds afterwards.
    fn published_beside(others: u64, warn_gb: u32, publish_now: bool, keeping: impl Fn(&Store) -> Keeping, save: &SaveFn<'_>) -> (PublishDone, u32, Totals) {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let read = std::cell::Cell::new(0);
        let totals = |store: &Store| {
            read.set(read.get() + 1);
            let here = store.totals();
            Totals { count: here.count + 3, bytes: here.bytes + others }
        };
        let limit = Limit { warn_gb, totals: &totals };
        let done = publish_job_with(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, publish_now, &keeping(&store), &Arc::new(Run::default()), &|_| {}, save, &limit).unwrap();
        (done, read.get(), store.totals())
    }

    fn kept_in(store: &Store) -> Keeping {
        Keeping::In(store.clone())
    }

    #[test]
    fn a_publish_that_crosses_the_limit_reports_it_and_still_succeeds() {
        // Exactly at the limit before, so this recording is what takes them past it.
        let (done, read, held) = published_beside(5 * GB, 5, true, kept_in, &store_save);
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }), "the tour is published all the same: {done:?}");
        assert_eq!((held.count, read), (1, 1), "the recording is saved, and the store was measured once");
        assert!(held.bytes > 0);
        assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: Some((5 * GB + held.bytes, 5)) });
        assert_eq!(publish_ui::kept_note(&done.kept, false), None, "the recording was kept whole");
        assert_eq!(publish_ui::over_note(&done.kept).as_deref(), Some("Saved recordings take 5.0 GB, more than the 5 GB you set. Nothing is removed automatically: remove recordings you no longer need in Preferences."));
        // A draft upload saves its recording as well, and says the same.
        let (draft, _, held) = published_beside(5 * GB, 5, false, kept_in, &store_save);
        assert_eq!((draft.finished.outcome.clone(), draft.kept), (Outcome::Draft, Kept::Saved { left_out: vec![], over: Some((5 * GB + held.bytes, 5)) }));
        // The first recording on a computer with a limit smaller than it: there was nothing before.
        let (first, _, held) = published_beside(0, 1, true, kept_in, &store_save);
        assert_eq!(first.kept, Kept::Saved { left_out: vec![], over: None }, "{} bytes are not a gigabyte", held.bytes);

        // Past the limit already, or still within it: saved, and nothing said.
        for others in [6 * GB, 5 * GB + 1, 4 * GB, 0] {
            let (done, read, held) = published_beside(others, 5, true, kept_in, &store_save);
            assert_eq!((done.kept, held.count, read), (Kept::Saved { left_out: vec![], over: None }, 1, 1), "{others}");
        }
        // No limit: saved, nothing said, and the saved recordings are not even measured.
        let (done, read, held) = published_beside(100 * GB, 0, true, kept_in, &store_save);
        assert_eq!((done.kept, held.count, read), (Kept::Saved { left_out: vec![], over: None }, 1, 0));

        // A recording that was not saved crossed nothing, and nothing is measured for it.
        let (off, read, held) = published_beside(5 * GB, 5, true, |_| Keeping::Off, &store_save);
        assert_eq!((off.kept, held.count, read), (Kept::Off, 0, 0));
        let (no_folder, read, _) = published_beside(5 * GB, 5, true, |_| Keeping::NoFolder, &store_save);
        assert_eq!((no_folder.kept, read), (Kept::Failed(publish_ui::NO_FOLDER_FOR_RECORDINGS.into()), 0));
        let (failed, read, held) = published_beside(5 * GB, 5, true, kept_in, &|_, _, _, _| Err("the disk is full.".to_string()));
        assert_eq!((failed.kept.clone(), held.count, read), (Kept::Failed("the disk is full.".into()), 0, 0));
        assert_eq!(publish_ui::over_note(&failed.kept), None);
        let (broke, read, _) = published_beside(5 * GB, 5, true, kept_in, &|_, _, _, _| panic!("the save broke"));
        assert_eq!((broke.kept, read), (Kept::Failed(publish_ui::KEEPING_BROKE_OFF.into()), 0));
    }

    #[test]
    fn a_measuring_that_breaks_says_nothing_and_the_recording_stays_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let s = publish_stub(|_| reply(200, LIVE));
        let state = state(tmp.path());
        let store = store_in(tmp.path());
        let nudges = crate::project::nudges_here();
        let limit = Limit { warn_gb: 5, totals: &|_| panic!("the folder could not be listed") };
        let done = publish_job_with(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}, &store_save, &limit).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }), "{done:?}");
        assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: None });
        assert_eq!((store.list().len(), crate::project::nudges_here()), (1, nudges + 1), "saved, and the conversion queue was told");
    }

    #[test]
    fn the_totals_are_read_on_the_job_thread_after_the_save() {
        // What happened, in order, with the thread it happened on and the projects the store held then.
        let log = Mutex::new(Vec::new());
        let note = |what: &'static str, store: &Store| log.lock().unwrap().push((what, std::thread::current().id(), store.list().len()));
        let (job_thread, done, requests) = std::thread::scope(|scope| {
            let job = scope.spawn(|| {
                let tmp = tempfile::tempdir().unwrap();
                let s = publish_stub(|_| reply(200, LIVE));
                let state = state(tmp.path());
                let store = store_in(tmp.path());
                let save: &SaveFn<'_> = &|store, key, snap, keep_going| {
                    note("save", store);
                    store_save(store, key, snap, keep_going)
                };
                let totals = |store: &Store| {
                    note("totals", store);
                    let here = store.totals();
                    Totals { count: here.count, bytes: here.bytes + 5 * GB }
                };
                let limit = Limit { warn_gb: 5, totals: &totals };
                let done = publish_job_with(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}, save, &limit);
                (std::thread::current().id(), done.unwrap(), s.lines())
            });
            job.join().unwrap()
        });
        assert_ne!(job_thread, std::thread::current().id());
        // Once, on the job's own thread, when the project was in the store.
        assert_eq!(*log.lock().unwrap(), vec![("save", job_thread, 0), ("totals", job_thread, 1)]);
        assert!(matches!(done.kept, Kept::Saved { over: Some((_, 5)), .. }), "{:?}", done.kept);
        // And the publish step came after both: the stub's publish route is the last request.
        assert!(requests.last().is_some_and(|l| l.contains(PUBLISH)), "{requests:?}");
    }

    #[test]
    fn an_update_never_reports_the_limit() {
        // The smallest limit there is, for as long as this test runs.
        let _alone = publish_ui::tests::SWITCH.lock().unwrap_or_else(|e| e.into_inner());
        let was = publish_ui::warn_gb();
        publish_ui::set_warn_gb(1);
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (store, key, _info, ed) = reopened_and_changed(tmp.path(), &s);
        let done = update_to(&s, &state_of(&ed), &store, &Arc::new(Run::default())).unwrap();
        publish_ui::set_warn_gb(was);
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }), "{done:?}");
        assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: None });
        assert_eq!(publish_ui::over_note(&done.kept), None);
        assert_eq!(store.list().iter().map(|p| &p.key).collect::<Vec<_>>(), vec![&key], "the same project, saved over");
        // The save of an update is handed no limit at all, so there is nothing it could measure:
        // the same save with one, as a new tour's, is what reports.
        let state = state_of(&ed);
        let up = Published { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 3, publish: None };
        let read = std::cell::Cell::new(0);
        let totals = |store: &Store| {
            read.set(read.get() + 1);
            Totals { count: 2, bytes: store.totals().bytes + GB }
        };
        let keep = |limit: Option<&Limit<'_>>| keep_recording(&Keeping::In(store.clone()), &store_save, limit, &|| true, &s.base, &up, &state, TITLE, TOUR_TITLE);
        assert_eq!((keep(None), read.get()), (Kept::Saved { left_out: vec![], over: None }, 0));
        assert!(matches!(keep(Some(&Limit { warn_gb: 1, totals: &totals })), Kept::Saved { over: Some((_, 1)), .. }));
        assert_eq!(read.get(), 1);
    }

    // ---- Update tour ----

    use crate::publish::tests::{Answer, committed, created, publish_stub_over};
    use std::sync::Mutex;

    /// What the test site does with a replace.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Replace {
        /// It works, and the commit says this about publishing.
        Works(&'static str),
        TourGone,
        ChapterGone,
        OldSite,
        /// Another upload replaced the chapter meanwhile.
        Changed,
        /// The commit gets no answer, twice.
        AnswerLost,
    }

    /// One site for a whole story: a new upload answers as the platform does (tour TOUR,
    /// chapter CH), and a replace of that chapter answers as `how` says at that moment, with
    /// step 1 already there. `at_commit` runs before every commit is answered, and `publish`
    /// answers the publish route.
    fn site(how: Arc<Mutex<Replace>>, at_commit: impl Fn() + Send + 'static, publish: impl Fn(usize) -> Answer + Send + 'static) -> Stub {
        let replacing = std::sync::atomic::AtomicBool::new(false);
        publish_stub_over(
            move |req, seen| {
                use std::sync::atomic::Ordering::SeqCst;
                let how = *how.lock().unwrap();
                if req.line == "POST /api/v1/uploads" {
                    let replace = serde_json::from_slice::<serde_json::Value>(&req.body).ok().is_some_and(|v| v["target"]["kind"] == "replace");
                    replacing.store(replace, SeqCst);
                    if replace {
                        return match how {
                            Replace::TourGone => reply(404, r#"{"error":"not_found","detail":"no such tour"}"#),
                            Replace::ChapterGone => reply(404, r#"{"error":"not_found","detail":"no such chapter"}"#),
                            Replace::OldSite => reply(400, r#"{"error":"bad_target","detail":"target.kind must be \"new\" or \"chapter\""}"#),
                            Replace::Works(_) | Replace::Changed | Replace::AnswerLost => reply(201, &created("[1]", false)),
                        };
                    }
                } else if req.line.starts_with("PUT ") {
                    return reply(204, "");
                } else if req.line.ends_with("/commit") {
                    at_commit();
                    if replacing.load(SeqCst) {
                        return match how {
                            Replace::Works(advice) => reply(201, &committed(advice)),
                            Replace::AnswerLost => Answer::Drop,
                            _ => reply(409, r#"{"error":"chapter_changed","detail":"the chapter was changed by another upload since this one started; start again"}"#),
                        };
                    }
                }
                platform(req, seen)
            },
            publish,
        )
    }

    fn working_site(advice: &'static str) -> Stub {
        site(Arc::new(Mutex::new(Replace::Works(advice))), || {}, |_| reply(200, LIVE))
    }

    fn target() -> EditTarget {
        EditTarget { tour_id: TOUR.into(), chapter_id: CH.into(), tour_title: TOUR_TITLE.into() }
    }

    /// What an update started now in `ed` would send and save, as `start_update` takes it.
    fn state_of(ed: &Editor) -> PublishState {
        let v = ed.video.as_ref().unwrap();
        PublishState { profile: String::new(), rec: v.rec.clone(), items: ed.doc.items().to_vec(), events: v.events.clone(), edit: v.edit.clone() }
    }

    /// Run the update job against `stub`, as the editor's job thread does, with the test token.
    fn update_to(stub: &Stub, state: &PublishState, store: &Store, run: &Arc<Run>) -> Result<PublishDone, String> {
        update_job(state, TITLE, &target(), &stub.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), run, &|_| {})
    }

    const NOTE: &str = "Added after it was published";

    /// A tour published to `stub` from a new recording and saved; then its project reopened
    /// and changed: a note that the saved project does not have.
    fn reopened_and_changed(root: &Path, stub: &Stub) -> (Store, ProjectKey, ProjectInfo, Editor) {
        let first = state(root);
        let store = store_in(root);
        assert_eq!(publish_to(stub, &first, false, &Keeping::In(store.clone())).unwrap().kept, Kept::Saved { left_out: vec![], over: None });
        drop(first);
        let key = key_at(stub);
        let info = store.list().remove(0);
        assert!(!project_text(&info).contains(NOTE));
        let mut ed = reopened(&store, &key);
        ed.add_item(Shape::Text { at: pt(60.0, 100.0), text: NOTE.into() });
        (store, key, info, ed)
    }

    /// Every file of a project with its bytes.
    fn files_of(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(dir).unwrap().map(|e| e.unwrap()) {
                if e.file_type().unwrap().is_dir() {
                    walk(&e.path(), base, out);
                } else {
                    out.push((e.path().strip_prefix(base).unwrap().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    #[test]
    fn a_successful_update_rewrites_the_saved_project() {
        for (advice, uploads_before_publish) in [("ready", true), ("draft", false), ("other_changes", false)] {
            let tmp = tempfile::tempdir().unwrap();
            // The publish route notes whether the saved project already had the change when it was called.
            let saved_at_publish = Arc::new(Mutex::new(Vec::new()));
            let (log, root) = (saved_at_publish.clone(), tmp.path().join("projects"));
            let s = site(Arc::new(Mutex::new(Replace::Works(advice))), || {}, move |_| {
                let text: Vec<String> = Store::at(root.clone()).list().iter().map(project_text).collect();
                log.lock().unwrap().push(text.len() == 1 && text[0].contains(NOTE));
                reply(200, LIVE)
            });
            let (store, key, info, ed) = reopened_and_changed(tmp.path(), &s);
            let requests = s.lines().len();

            let done = update_to(&s, &state_of(&ed), &store, &Arc::new(Run::default())).unwrap();
            assert_eq!((done.finished.tour_id.as_str(), done.finished.chapter_id.as_str()), (TOUR, CH));
            match advice {
                "ready" => assert!(matches!(done.finished.outcome, Outcome::Public { .. }), "{done:?}"),
                "draft" => assert_eq!(done.finished.outcome, Outcome::Draft),
                _ => assert_eq!(done.finished.outcome, Outcome::Held),
            }
            // The site got a replace of exactly that chapter, and only the pictures it lacked.
            let reqs = s.requests();
            let create: serde_json::Value = serde_json::from_slice(&reqs[requests].body).unwrap();
            assert_eq!(create["target"], serde_json::json!({ "kind": "replace", "tour_id": TOUR, "chapter_id": CH }));
            let sent: Vec<&str> = reqs[requests..].iter().map(|r| r.line.as_str()).filter(|l| l.starts_with("PUT ")).collect();
            assert_eq!(sent.len(), 2, "step 1 is on the site already: {sent:?}");
            assert!(sent[0].ends_with("/steps/2") && sent[1].ends_with("/steps/3"));
            // The project is rewritten, for every outcome, and before anything was published.
            assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: None }, "{advice}");
            assert_eq!(*saved_at_publish.lock().unwrap(), if uploads_before_publish { vec![true] } else { vec![] }, "{advice}");
            let list = store.list();
            assert_eq!(list.len(), 1, "the same project, not a second one");
            assert_eq!((&list[0].key, &list[0].dir, list[0].problem), (&key, &info.dir, None));
            let text = project_text(&list[0]);
            assert!(text.contains(NOTE) && !text.contains(TOKEN), "{text}");
            // It stays the editor's origin, open in it; closed and opened again, the change is there.
            assert_eq!(ed.video.as_ref().unwrap().origin.as_ref().unwrap().key, key);
            assert!(matches!(store.load(&key), Err(LoadError::AlreadyOpen)));
            drop(ed);
            let again = reopened(&store, &key);
            assert_eq!(again.doc.items().len(), 3);
            assert!(again.doc.items().iter().any(|it| matches!(&it.shape, Shape::Text { text, .. } if text == NOTE)));
        }
    }

    #[test]
    fn a_cancelled_or_failed_update_leaves_the_saved_project_byte_for_byte_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let how = Arc::new(Mutex::new(Replace::Works("ready")));
        let s = site(how.clone(), || {}, |_| reply(200, LIVE));
        let (store, key, info, ed) = reopened_and_changed(tmp.path(), &s);
        let state = state_of(&ed);
        let before = files_of(&info.dir);
        let unchanged = |what: &str| {
            assert_eq!(files_of(&info.dir), before, "{what}");
            assert_eq!(store.list(), std::slice::from_ref(&info), "{what}");
        };

        // Cancelled before anything is sent, and cancelled while the pictures go up.
        let run = Arc::new(Run::default());
        run.cancel();
        assert_eq!(update_to(&s, &state, &store, &run), Err(CANCELLED.to_string()));
        unchanged("cancelled at once");
        let run = Arc::new(Run::default());
        let commits = s.lines().iter().filter(|l| l.ends_with("/commit")).count();
        let cancelled = update_job(&state, TITLE, &target(), &s.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), &run, &|f| {
            if f > PUBLISH_WRITE_SHARE {
                run.cancel();
            }
        });
        assert_eq!(cancelled, Err(CANCELLED.to_string()));
        assert_eq!(s.lines().iter().filter(|l| l.ends_with("/commit")).count(), commits, "it never reached the commit");
        assert_eq!(failed_publish_message(CANCELLED, true), "Update cancelled");
        unchanged("cancelled mid-upload");

        // No token: nothing is sent.
        let sent = s.lines().len();
        let no_token = update_job(&state, TITLE, &target(), &s.base, || Err("Add the server and token first.".to_string()), &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {});
        assert_eq!(no_token, Err("Add the server and token first.".to_string()));
        assert_eq!(s.lines().len(), sent);
        unchanged("no token");

        // The site refuses, each with its own sentence; the dialog then offers Update again or not.
        let refusals = [
            (Replace::TourGone, "Could not start the upload: that tour no longer exists.", false),
            (Replace::ChapterGone, "Could not start the upload: that chapter is no longer part of the tour.", false),
            (Replace::OldSite, "Could not start the upload: the site needs an update to version 0.1.4 or newer.", false),
            (Replace::Changed, "Could not finish the upload: the chapter was changed by another upload meanwhile. Update again.", true),
        ];
        for (refusal, sentence, again) in refusals {
            *how.lock().unwrap() = refusal;
            let e = update_to(&s, &state, &store, &Arc::new(Run::default())).unwrap_err();
            assert_eq!(e, sentence);
            assert_eq!(failed_publish_message(&e, true), sentence, "the status bar says the sentence");
            assert_eq!(publish_ui::update_can_be_tried_again(&e), again, "{sentence}");
            assert!(!e.contains(TOKEN));
            unchanged(sentence);
        }

        // The commit's answer is lost: the site may hold the update or not, so the saved
        // project is not rewritten on a guess.
        *how.lock().unwrap() = Replace::AnswerLost;
        let e = update_to(&s, &state, &store, &Arc::new(Run::default())).unwrap_err();
        assert!(e.starts_with("Could not finish the upload: ") && e.ends_with(" Check the tour list before uploading again."), "{e}");
        assert!(publish_ui::update_can_be_tried_again(&e));
        unchanged("the commit answer was lost");
        assert!(!s.lines().iter().any(|l| l == PUBLISH), "nothing was ever published");

        // The project is still this editor's, still opens, and still lacks the change that was never sent.
        assert_eq!(ed.video.as_ref().unwrap().origin.as_ref().unwrap().key, key);
        drop(state);
        drop(ed);
        assert_eq!(reopened(&store, &key).doc.items().len(), 2);
    }

    #[test]
    fn a_saved_recording_that_cannot_be_rewritten_never_fails_the_update() {
        for advice in ["ready", "draft", "other_changes"] {
            let tmp = tempfile::tempdir().unwrap();
            let s = working_site(advice);
            let (store, _key, info, ed) = reopened_and_changed(tmp.path(), &s);
            let state = state_of(&ed);
            let before = files_of(&info.dir);
            // The save fails with the store's reason.
            let asked = std::cell::Cell::new(0);
            let failing: &SaveFn<'_> = &|_, _, _, _| {
                asked.set(asked.get() + 1);
                Err("the project file could not be written.".to_string())
            };
            let done = update_job_with(&state, TITLE, &target(), &s.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}, failing).unwrap();
            assert_eq!(asked.get(), 1, "one save, whatever the site says about publishing");
            let published = s.lines().iter().filter(|l| *l == PUBLISH).count();
            match advice {
                "ready" => assert!(matches!(done.finished.outcome, Outcome::Public { .. }) && published == 1, "{done:?}"),
                "draft" => assert!(done.finished.outcome == Outcome::Draft && published == 0),
                _ => assert!(done.finished.outcome == Outcome::Held && published == 0),
            }
            assert_eq!(done.kept, Kept::Failed("the project file could not be written.".into()));
            assert_eq!(
                publish_ui::kept_note(&done.kept, true).as_deref(),
                Some("The changes were uploaded, but the recording saved on this computer could not be updated, so it no longer matches the tour: the project file could not be written.")
            );
            assert_eq!(files_of(&info.dir), before);

            // A save that panics: the same, with the internal-error reason, and the publish step still ran.
            let broken: &SaveFn<'_> = &|_, _, _, _| panic!("the save broke");
            let done = update_job_with(&state, TITLE, &target(), &s.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}, broken).unwrap();
            assert_eq!(done.kept, Kept::Failed(publish_ui::KEEPING_BROKE_OFF.into()));
            assert_eq!(s.lines().iter().filter(|l| *l == PUBLISH).count(), if advice == "ready" { 2 } else { 0 });
            assert!(publish_ui::kept_note(&done.kept, true).is_some_and(|n| n.starts_with(publish_ui::UPDATE_NOT_SAVED)));

            // No folder for saved recordings: said as a recording that no longer matches, too.
            let done = update_job(&state, TITLE, &target(), &s.base, || Ok(TOKEN.to_string()), &Keeping::NoFolder, &Arc::new(Run::default()), &|_| {}).unwrap();
            assert_eq!(done.kept, Kept::Failed(publish_ui::NO_FOLDER_FOR_RECORDINGS.into()));
            assert_eq!(files_of(&info.dir), before);
        }
    }

    #[test]
    fn cancel_pressed_at_the_commit_of_an_update_still_saves_the_project_and_publishes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (store, key, _info, ed) = reopened_and_changed(tmp.path(), &s);
        let run = Arc::new(Run::default());
        let pressed = run.clone();
        // Cancel is pressed when the commit has answered and the save begins. The save is asked
        // whether to go on, as a copy of frames would ask: the answer is yes.
        let go_on = std::cell::Cell::new(None);
        let asking: &SaveFn<'_> = &|store, key, snap, keep_going| {
            pressed.cancel();
            go_on.set(Some(keep_going()));
            store.save(key, snap)
        };
        let done = update_job_with(&state_of(&ed), TITLE, &target(), &s.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), &run, &|_| {}, asking).unwrap();
        assert_eq!(go_on.get(), Some(true), "the site has the changes: the save is not cut short");
        assert_eq!(done.finished.outcome, Outcome::NotPublic { cancelled: true, error: String::new() });
        assert!(!s.lines().iter().any(|l| l == PUBLISH));
        assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: None });
        assert!(project_text(&store.list()[0]).contains(NOTE));
        assert_eq!(store.list()[0].key, key);
        let was = Before { public: true, link: None, steps: Some(2) };
        assert_eq!(publish_ui::result_status(&done.finished, &was, true), "Update uploaded: the public page still shows the previous version");
        assert_eq!(publish_ui::result_note(&done.finished.outcome, &was, true), Some(("Publishing was cancelled. The update is uploaded, but the public page still shows the previous version.".to_string(), false)));
    }

    #[test]
    fn try_again_after_an_update_publishes_only_and_neither_uploads_nor_saves() {
        let tmp = tempfile::tempdir().unwrap();
        let s = site(Arc::new(Mutex::new(Replace::Works("ready"))), || {}, |calls| if calls == 0 { reply(500, r#"{"error":"publish_failed","detail":"the tour could not be published"}"#) } else { reply(200, LIVE) });
        let (store, _key, info, ed) = reopened_and_changed(tmp.path(), &s);
        let done = update_to(&s, &state_of(&ed), &store, &Arc::new(Run::default())).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::NotPublic { cancelled: false, .. }), "{done:?}");
        let saved = files_of(&info.dir);
        let was = Before { public: true, link: None, steps: Some(2) };
        let mut screen = Some(ResultDialog::after_update(TOUR_TITLE.into(), was, done));
        let retry = publish_ui::take_retry(&mut screen, false).unwrap();
        assert_eq!((retry.tour_id.as_str(), retry.chapter_id.as_str(), retry.update), (TOUR, CH, true));
        let sent = s.lines().len();
        let again = retry_done(&retry, crate::publish::publish_tour(&s.base, TOKEN, &retry.tour_id, |_| true));
        assert!(matches!(again.finished.outcome, Outcome::Public { .. }), "{again:?}");
        assert_eq!((again.finished.chapter_id.as_str(), &again.kept), (CH, &Kept::Saved { left_out: vec![], over: None }));
        assert_eq!(&s.lines()[sent..], [PUBLISH], "one publish call, and nothing else");
        assert_eq!(files_of(&info.dir), saved, "the project was not written a second time");
    }

    #[test]
    fn publish_as_a_new_tour_from_an_edit_leaves_the_first_tours_project_alone_and_saves_a_second() {
        const OTHER_TOUR: &str = "tour_bbbbbbbbbbbbbbbbbbbbbbbb";
        const OTHER_CH: &str = "ch_bbbbbbbbbbbbbbbbbbbbbbbb";
        let tmp = tempfile::tempdir().unwrap();
        // The site's first commit makes TOUR; every later one makes another tour.
        let s = stub(|req, seen| {
            if req.line.starts_with("PUT ") {
                reply(204, "")
            } else if req.line.ends_with("/commit") && seen > 0 {
                reply(201, &format!(r#"{{"tour_id":"{OTHER_TOUR}","chapter_id":"{OTHER_CH}","steps":2}}"#))
            } else {
                platform(req, seen)
            }
        });
        let (store, key, info, ed) = reopened_and_changed(tmp.path(), &s);
        let before = files_of(&info.dir);
        let sent = s.lines().len();
        // "Publish as a new tour" is the publish of any recording, with a new tour as its target.
        let done = publish_job(&state_of(&ed), "A second tour", "A second tour", &s.base, || Ok(TOKEN.to_string()), &Target::New, false, &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}).unwrap();
        assert_eq!((done.finished.tour_id.as_str(), done.finished.chapter_id.as_str(), &done.kept), (OTHER_TOUR, OTHER_CH, &Kept::Saved { left_out: vec![], over: None }));
        let create: serde_json::Value = serde_json::from_slice(&s.requests()[sent].body).unwrap();
        assert_eq!(create["target"], serde_json::json!({ "kind": "new" }));
        assert!(create.get("sha256").is_none());
        // The first tour's project is as it was, and a second one holds the change.
        assert_eq!(files_of(&info.dir), before);
        let other = ProjectKey { server: key.server.clone(), tour_id: OTHER_TOUR.into(), chapter_id: OTHER_CH.into() };
        let mut keys: Vec<ProjectKey> = store.list().iter().map(|p| p.key.clone()).collect();
        keys.sort_by(|a, b| a.tour_id.cmp(&b.tour_id));
        assert_eq!(keys, [key.clone(), other.clone()]);
        let second = store.for_tour(&key.server, OTHER_TOUR).remove(0);
        assert_eq!((second.title.as_str(), second.tour_title.as_str(), second.problem), ("A second tour", "A second tour", None));
        assert!(project_text(&second).contains(NOTE));
        // The origin does not move: the editor still edits the first tour's project.
        assert_eq!(ed.video.as_ref().unwrap().origin.as_ref().unwrap().key, key);
        assert_eq!(edit_target(ed.video.as_ref().unwrap().origin.as_ref().unwrap()), target());
        assert!(matches!(store.load(&key), Err(LoadError::AlreadyOpen)));
        // The second one is a recording of its own: it opens while the first is open, and after it closed.
        let second_ed = reopened(&store, &other);
        assert_eq!(second_ed.doc.items().len(), 3);
        drop(second_ed);
        drop(ed);
        assert_eq!(reopened(&store, &key).doc.items().len(), 2);
        drop(reopened(&store, &other));
    }

    // ---- Publishing again from the editor that published: replace the chapter it uploaded ----

    fn pending(server: &str) -> Pending {
        Pending { profile: String::new(), server: server.into(), chapter_title: "Typed chapter".into() }
    }

    #[test]
    fn an_upload_is_remembered_as_the_saved_project_is_keyed() {
        let done = |kept: Kept, outcome: Outcome| PublishDone { finished: Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 2, outcome }, kept };
        let saved = Kept::Saved { left_out: vec![], over: None };
        // The address in the form the address rule writes, as `keep_recording` keys the project.
        let r = remember_upload(&pending(" https://tours.example.com/ "), TOUR_TITLE, &done(saved.clone(), Outcome::Draft), None).unwrap();
        assert_eq!(r, Remembered { profile: String::new(), key: ProjectKey { server: "https://tours.example.com".into(), tour_id: TOUR.into(), chapter_id: CH.into() }, chapter_title: "Typed chapter".into(), tour_title: TOUR_TITLE.into(), kept: true });
        // Published, or uploaded and not published: the chapter is on the site either way.
        for outcome in [Outcome::Public { url: None }, Outcome::NotPublic { cancelled: true, error: String::new() }] {
            assert!(remember_upload(&pending("https://tours.example.com"), TOUR_TITLE, &done(Kept::Off, outcome), None).is_some_and(|r| !r.kept));
        }
        // A tour taken away again after a restricted publish did not finish: nothing to replace.
        assert_eq!(remember_upload(&pending("https://tours.example.com"), TOUR_TITLE, &done(saved.clone(), Outcome::Draft), Some(publish_ui::Left::Deleted)), None);
        let mut no_chapter = done(saved, Outcome::Draft);
        no_chapter.finished.chapter_id.clear();
        assert_eq!(remember_upload(&pending("https://tours.example.com"), TOUR_TITLE, &no_chapter, None), None);
    }

    /// A profile on the test site.
    fn on_site(id: &str, name: &str, server: &str) -> crate::profiles::Profile {
        crate::profiles::Profile { id: id.into(), name: name.into(), server: server.into(), account: None, workspace: None, identity: None, state: crate::profiles::State::Ready, named: true }
    }

    #[test]
    fn an_update_belongs_to_the_profile_the_tour_was_uploaded_with() {
        use crate::profiles::{Book, MIGRATED_ID};
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let mut ed = editor_of(&st);
        let server = "https://tours.example.com";
        // Two profiles on the same site: the address alone does not decide.
        let book = |active: &str| Book { list: vec![on_site(MIGRATED_ID, "Robin · mythicshot.com", server), on_site("pb", "Sandbox admin", server)], active: Some(active.into()), legacy: Some(MIGRATED_ID.into()) };
        let key = ProjectKey { server: server.into(), tour_id: TOUR.into(), chapter_id: CH.into() };
        let target = EditTarget { tour_id: TOUR.into(), chapter_id: CH.into(), tour_title: TOUR_TITLE.into() };
        let replace = Dialog::Replace { title: "Typed chapter".into(), target: target.clone(), logo_missing: false };
        ed.video.as_mut().unwrap().remembered = Some(Remembered { profile: "pb".into(), key, chapter_title: "Typed chapter".into(), tour_title: TOUR_TITLE.into(), kept: true });
        let v = ed.video.as_ref().unwrap();
        assert_eq!(dialog_in(v, server, TITLE.into(), &book("pb")), replace);
        // Another profile is active: refused with the question, before any dialog.
        assert_eq!(dialog_in(v, server, TITLE.into(), &book(MIGRATED_ID)), Dialog::OtherProfile { owner: Owner::Other { id: "pb".into(), name: "Sandbox admin".into() }, title: TITLE.into() });
        // Its profile was removed: said so, and only a new tour.
        let mut gone = book(MIGRATED_ID);
        gone.list.retain(|p| p.id != "pb");
        assert_eq!(dialog_in(v, server, TITLE.into(), &gone), Dialog::OtherProfile { owner: Owner::Removed, title: TITLE.into() });
        // Linked before profiles: the first profile's tour.
        ed.video.as_mut().unwrap().remembered.as_mut().unwrap().profile.clear();
        let v = ed.video.as_ref().unwrap();
        assert_eq!(dialog_in(v, server, TITLE.into(), &book(MIGRATED_ID)), replace);
        assert!(matches!(dialog_in(v, server, TITLE.into(), &book("pb")), Dialog::OtherProfile { owner: Owner::Other { .. }, .. }));
        // No profile at all: as before profiles (and nothing can be sent anyway).
        assert_eq!(dialog_in(v, server, TITLE.into(), &Book::default()), replace);
        // A new recording belongs to nobody yet.
        let fresh = editor_of(&state(&tmp.path().join("fresh")));
        assert_eq!(dialog_in(fresh.video.as_ref().unwrap(), server, TITLE.into(), &book(MIGRATED_ID)), Dialog::New(TITLE.into()));
    }

    #[test]
    fn an_update_of_a_recording_nobody_owns_yet_goes_by_its_address_and_stamps_the_profile() {
        use crate::profiles::Book;
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (store, key, info, ed) = reopened_and_changed(tmp.path(), &s);
        assert!(info.profile.is_empty(), "kept before profiles");
        // No connection was carried over: the recording is nobody's, not a removed profile's.
        let book = |server: &str| Book { list: vec![on_site("pb", "Sandbox admin", server)], active: Some("pb".into()), legacy: None };
        let v = ed.video.as_ref().unwrap();
        assert!(matches!(dialog_in(v, &s.base, TITLE.into(), &book(&s.base)), Dialog::Replace { .. }), "the active profile at its own address may update it");
        assert!(matches!(dialog_in(v, "https://other.example.com", TITLE.into(), &book("https://other.example.com")), Dialog::Elsewhere { .. }), "at another address only a new tour or chapter");
        // The update's save stamps the profile it went with.
        let mut state = state_of(&ed);
        state.profile = "pb".into();
        update_to(&s, &state, &store, &Arc::new(Run::default())).unwrap();
        assert_eq!(store.list().into_iter().find(|i| i.key == key).unwrap().profile, "pb");
    }

    #[test]
    fn update_tour_runs_for_a_recording_nobody_owns_yet_at_its_own_address() {
        use crate::profiles::{Book, Pin};
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (_store, _key, info, mut ed) = reopened_and_changed(tmp.path(), &s);
        assert!(info.profile.is_empty());
        let book = Book { list: vec![on_site("pb", "Sandbox admin", &s.base)], active: Some("pb".into()), legacy: None };
        let pin = Pin { profile: Some("pb".into()), server: s.base.clone() };
        let asked: Arc<std::sync::Mutex<Vec<Pin>>> = Arc::default();
        let record = asked.clone();
        let token: Arc<dyn Fn(&Pin) -> Result<String, String> + Send + Sync> = Arc::new(move |p: &Pin| {
            record.lock().unwrap().push(p.clone());
            Ok(TOKEN.to_string())
        });
        // Another profile's tour (by its stamp) is refused on this path too.
        ed.video.as_mut().unwrap().origin.as_mut().unwrap().profile = "pother".into();
        ed.start_update_with(TITLE.into(), target(), Before::new_tour(), pin.clone(), &book, token.clone());
        assert!(!ed.exporting());
        // Nobody's yet: the active profile at the recording's own address updates it.
        ed.video.as_mut().unwrap().origin.as_mut().unwrap().profile.clear();
        ed.start_update_with(TITLE.into(), target(), Before::new_tour(), pin.clone(), &book, token);
        assert!(ed.exporting(), "Update tour starts");
        until_done(&mut ed);
        assert_eq!(ed.video.as_ref().unwrap().job_pin.as_ref(), Some(&pin));
        assert_eq!(*asked.lock().unwrap(), vec![pin]);
        assert!(s.lines().iter().any(|l| l.ends_with("/commit")), "{:?}", s.lines());
    }

    #[test]
    fn an_update_holds_its_profiles_token_from_before_it_reads_it_until_it_ends() {
        use crate::profiles::{Book, Pin};
        const ID: &str = "p-update-job-hold";
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (_store, _key, _info, mut ed) = reopened_and_changed(tmp.path(), &s);
        let book = Book { list: vec![on_site(ID, "Sandbox admin", &s.base)], active: Some(ID.into()), legacy: None };
        let pin = Pin { profile: Some(ID.into()), server: s.base.clone() };
        let seen: Arc<std::sync::Mutex<Vec<bool>>> = Arc::default();
        let record = seen.clone();
        let token: Arc<dyn Fn(&Pin) -> Result<String, String> + Send + Sync> = Arc::new(move |_: &Pin| {
            record.lock().unwrap().push(crate::profiles::holds::global().is_held(ID));
            Ok(TOKEN.to_string())
        });
        ed.start_update_with(TITLE.into(), target(), Before::new_tour(), pin, &book, token);
        assert!(ed.exporting(), "Update tour starts");
        until_done(&mut ed);
        let seen = seen.lock().unwrap().clone();
        assert!(!seen.is_empty() && seen.iter().all(|h| *h), "held whenever the token was read: {seen:?}");
        assert!(!crate::profiles::holds::global().is_held(ID), "let go when the job ended");
    }

    #[test]
    fn a_kept_recording_is_linked_to_its_tour_in_the_profile_it_was_uploaded_with() {
        let tmp = tempfile::tempdir().unwrap();
        let s = stub(platform);
        let store = store_in(tmp.path());
        let mut st = state(tmp.path());
        st.profile = "pb".into();
        assert!(matches!(publish_to(&s, &st, false, &Keeping::In(store.clone())).unwrap().kept, Kept::Saved { .. }));
        let info = store.list().remove(0);
        assert_eq!(info.profile, "pb");
        assert!(project_text(&info).contains("\"profile\": \"pb\""), "{}", project_text(&info));
        // Opened again through Edit, the editor knows whose tour it is.
        let ed = reopened(&store, &info.key);
        assert_eq!(ed.video.as_ref().unwrap().origin.as_ref().unwrap().profile, "pb");
        assert_eq!(replaceable(ed.video.as_ref().unwrap()).unwrap().profile, "pb");
        // A recording kept before profiles has no profile in its file at all.
        let tmp = tempfile::tempdir().unwrap();
        let store = store_in(tmp.path());
        let s = stub(platform);
        assert!(matches!(publish_to(&s, &state(tmp.path()), false, &Keeping::In(store.clone())).unwrap().kept, Kept::Saved { .. }));
        let info = store.list().remove(0);
        assert!(info.profile.is_empty() && !project_text(&info).contains("\"profile\""));
    }

    /// Run `ed`'s frames until its job has ended.
    fn until_done(ed: &mut Editor) {
        let ctx = crate::theme::test_context();
        for _ in 0..500 {
            tour_frame(ed, &ctx, vec![]);
            if !ed.exporting() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the job did not end");
    }

    #[test]
    fn an_editor_that_published_offers_to_replace_that_chapter_next_time() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let mut ed = editor_of(&st);
        let server = "https://tours.example.com";
        // A new recording: a new tour or a new chapter, nothing to replace.
        assert_eq!(dialog_for(ed.video.as_ref().unwrap(), server, TITLE.into()), Dialog::New(TITLE.into()));
        // A publish that failed before uploading remembers nothing.
        let failing = EncodeJob::spawn(|_| Err::<PublishDone, String>("Could not start the upload: the server could not be reached.".into()));
        let v = ed.video.as_mut().unwrap();
        v.job = Some(ExportJob::Publish(failing, Arc::new(Run::default()), TOUR_TITLE.into(), Before::new_tour(), false));
        v.publishing_to = Some(pending(server));
        until_done(&mut ed);
        let v = ed.video.as_ref().unwrap();
        assert_eq!((&v.remembered, &v.publishing_to), (&None, &None));
        // A publish that uploaded: the next Publish dialog replaces that chapter, under the title it was uploaded with.
        let (job, go, _run) = waiting_job(|| Kept::Saved { left_out: vec![], over: None });
        let v = ed.video.as_mut().unwrap();
        v.job = Some(job);
        v.publishing_to = Some(pending(server));
        go.send(()).unwrap();
        until_done(&mut ed);
        let v = ed.video.as_ref().unwrap();
        let key = ProjectKey { server: server.into(), tour_id: TOUR.into(), chapter_id: CH.into() };
        assert_eq!(v.remembered.as_ref().map(|r| &r.key), Some(&key));
        let target = EditTarget { tour_id: TOUR.into(), chapter_id: CH.into(), tour_title: TOUR_TITLE.into() };
        assert_eq!(dialog_for(v, server, TITLE.into()), Dialog::Replace { title: "Typed chapter".into(), target: target.clone(), logo_missing: false });
        assert_eq!(dialog_for(v, " https://tours.example.com/ ", TITLE.into()), Dialog::Replace { title: "Typed chapter".into(), target: target.clone(), logo_missing: false });
        // Another site address configured since: no Replace, as for an opened project.
        assert_eq!(dialog_for(v, "https://other.example.com", TITLE.into()), Dialog::Elsewhere { title: "Typed chapter".into(), target: target.clone() });
        // An update of it keeps it, under the tour's title as the site had it.
        let r = replaceable(v).unwrap();
        assert!(r.keep_always && is_chapter(&target, &r.key));
        let update = EncodeJob::spawn(|_| Ok(PublishDone { finished: Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 3, outcome: Outcome::Draft }, kept: Kept::Saved { left_out: vec![], over: None } }));
        ed.video.as_mut().unwrap().job = Some(ExportJob::Publish(update, Arc::new(Run::default()), "Renamed on the site".into(), Before::new_tour(), true));
        until_done(&mut ed);
        let v = ed.video.as_ref().unwrap();
        assert_eq!(v.remembered.as_ref().map(|r| (&r.key, r.tour_title.as_str(), r.chapter_title.as_str())), Some((&key, "Renamed on the site", "Typed chapter")));
    }

    #[test]
    fn a_publish_from_an_opened_project_moves_replace_to_the_new_chapter_and_keeps_the_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (_store, key, _info, mut ed) = reopened_and_changed(tmp.path(), &s);
        let v = ed.video.as_ref().unwrap();
        assert_eq!(replaceable(v).map(|r| (r.key, r.chapter_title, r.keep_always)), Some((key.clone(), None, true)), "the opened project, under the editor's title");
        // It is published as a new tour: Replace is that tour's chapter from now on.
        const OTHER_TOUR: &str = "tour_bbbbbbbbbbbbbbbbbbbbbbbb";
        let job = EncodeJob::spawn(|_| Ok(PublishDone { finished: Finished { tour_id: OTHER_TOUR.into(), chapter_id: CH.into(), steps: 2, outcome: Outcome::Draft }, kept: Kept::Off }));
        let v = ed.video.as_mut().unwrap();
        v.job = Some(ExportJob::Publish(job, Arc::new(Run::default()), "A second tour".into(), Before::new_tour(), false));
        v.publishing_to = Some(Pending { profile: String::new(), server: s.base.clone(), chapter_title: "A second tour".into() });
        until_done(&mut ed);
        let v = ed.video.as_ref().unwrap();
        let r = replaceable(v).unwrap();
        assert_eq!((r.target.tour_id.as_str(), r.target.tour_title.as_str(), r.chapter_title.as_deref()), (OTHER_TOUR, "A second tour", Some("A second tour")));
        assert!(!r.keep_always, "its recording was not kept: a replace follows the keep setting");
        assert_eq!(v.origin.as_ref().unwrap().key, key, "the project it was opened from stays its origin");
    }

    #[test]
    fn a_chapter_the_site_no_longer_has_is_not_offered_to_replace_again() {
        const GONE: &str = "Could not start the upload: that chapter is no longer part of the tour.";
        let tmp = tempfile::tempdir().unwrap();
        let st = state(tmp.path());
        let mut ed = editor_of(&st);
        let server = "https://tours.example.com";
        let key = ProjectKey { server: server.into(), tour_id: TOUR.into(), chapter_id: CH.into() };
        ed.video.as_mut().unwrap().remembered = Some(Remembered { profile: String::new(), key: key.clone(), chapter_title: TITLE.into(), tour_title: TOUR_TITLE.into(), kept: true });
        let target = EditTarget { tour_id: TOUR.into(), chapter_id: CH.into(), tour_title: TOUR_TITLE.into() };
        // Only the site saying the chapter is gone counts: a gone tour shows in every new
        // dialog's list, and anything else may work the next time.
        let r = replaceable(ed.video.as_ref().unwrap());
        assert_eq!(gone_after(GONE, r.as_ref()), Some(key.clone()));
        for other in ["Could not start the upload: that tour no longer exists.", "Could not finish the upload: the chapter was changed by another upload meanwhile. Update again.", "Could not send step 3: the server could not be reached."] {
            assert_eq!(gone_after(other, r.as_ref()), None, "{other}");
        }
        // An update that ends that way, in the editor. (A screen is open, so no dialog is made
        // here: making one would ask a site.)
        let failing = EncodeJob::spawn(|_| Err::<PublishDone, String>(GONE.into()));
        let v = ed.video.as_mut().unwrap();
        v.publish_result = Some(ResultDialog::new(TOUR_TITLE.into(), Before::new_tour(), PublishDone { finished: Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 1, outcome: Outcome::Draft }, kept: Kept::Off }));
        v.job = Some(ExportJob::Publish(failing, Arc::new(Run::default()), TOUR_TITLE.into(), Before::new_tour(), true));
        until_done(&mut ed);
        let v = ed.video.as_ref().unwrap();
        assert_eq!(dialog_for(v, server, TITLE.into()), Dialog::ChapterGone { title: TITLE.into(), target: target.clone() }, "a new chapter of its tour, or a new tour");
        assert!(replaceable(v).is_some_and(|r| r.gone));
        // Asked to replace it all the same, nothing starts.
        ed.start_update(TITLE.into(), target.clone(), Before::new_tour());
        assert!(!ed.exporting());
        // A new upload from this editor is a chapter the site has: Replace is offered for it.
        let other = ProjectKey { chapter_id: "ch_bbbbbbbbbbbbbbbbbbbbbbbb".into(), ..key };
        ed.video.as_mut().unwrap().remembered = Some(Remembered { profile: String::new(), key: other, chapter_title: TITLE.into(), tour_title: TOUR_TITLE.into(), kept: true });
        assert!(matches!(dialog_for(ed.video.as_ref().unwrap(), server, TITLE.into()), Dialog::Replace { .. }));
    }

    #[test]
    fn replacing_what_a_new_recording_published_saves_over_the_same_project() {
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("draft");
        let store = store_in(tmp.path());
        let first = state(tmp.path());
        let done = publish_to(&s, &first, false, &Keeping::In(store.clone())).unwrap();
        let r = remember_upload(&Pending { profile: String::new(), server: s.base.clone(), chapter_title: TITLE.into() }, TOUR_TITLE, &done, None).unwrap();
        assert_eq!(r.key, key_at(&s), "keyed as the saved project is");
        let mut ed = editor_of(&first);
        ed.video.as_mut().unwrap().edit = first.edit.clone();
        ed.video.as_mut().unwrap().remembered = Some(r.clone());
        ed.add_item(Shape::Text { at: pt(60.0, 100.0), text: NOTE.into() });
        let target = replaceable(ed.video.as_ref().unwrap()).unwrap().target;
        let sent = s.lines().len();
        let again = update_job(&state_of(&ed), &r.chapter_title, &target, &s.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}).unwrap();
        assert_eq!((again.finished.tour_id.as_str(), again.finished.chapter_id.as_str()), (TOUR, CH));
        let create: serde_json::Value = serde_json::from_slice(&s.requests()[sent].body).unwrap();
        assert_eq!(create["target"], serde_json::json!({ "kind": "replace", "tour_id": TOUR, "chapter_id": CH }), "the same chapter, not a second one");
        let list = store.list();
        assert_eq!(list.len(), 1, "the same project, saved over");
        assert_eq!((&list[0].key, list[0].title.as_str()), (&key_at(&s), TITLE));
        assert!(project_text(&list[0]).contains(NOTE));
    }

    #[test]
    fn a_remembered_chapter_removed_on_the_site_fails_the_replace_and_loses_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let how = Arc::new(Mutex::new(Replace::Works("draft")));
        let s = site(how.clone(), || {}, |_| reply(200, LIVE));
        let store = store_in(tmp.path());
        let first = state(tmp.path());
        let done = publish_to(&s, &first, false, &Keeping::In(store.clone())).unwrap();
        let r = remember_upload(&Pending { profile: String::new(), server: s.base.clone(), chapter_title: TITLE.into() }, TOUR_TITLE, &done, None).unwrap();
        let mut ed = editor_of(&first);
        ed.video.as_mut().unwrap().remembered = Some(r.clone());
        ed.add_item(Shape::Text { at: pt(60.0, 100.0), text: NOTE.into() });
        let items = ed.doc.items().to_vec();
        let before = files_of(&store.list()[0].dir);
        let target = replaceable(ed.video.as_ref().unwrap()).unwrap().target;
        for (refusal, sentence, again) in [
            (Replace::ChapterGone, "Could not start the upload: that chapter is no longer part of the tour.", false),
            (Replace::TourGone, "Could not start the upload: that tour no longer exists.", false),
            (Replace::Changed, "Could not finish the upload: the chapter was changed by another upload meanwhile. Update again.", true),
        ] {
            *how.lock().unwrap() = refusal;
            let e = update_job(&state_of(&ed), &r.chapter_title, &target, &s.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}).unwrap_err();
            assert_eq!((e.as_str(), publish_ui::update_can_be_tried_again(&e)), (sentence, again));
            // The editor's work and what it remembers are as they were, and so is the saved project.
            assert_eq!(ed.doc.items(), items.as_slice());
            assert_eq!(ed.video.as_ref().unwrap().remembered.as_ref(), Some(&r));
            assert_eq!(files_of(&store.list()[0].dir), before);
        }
    }

    #[test]
    fn updating_shows_its_progress_and_then_the_publish_count() {
        assert_eq!(progress_message(true, Some(false), false, None, true, 0.4), "Updating tour… 40%");
        assert_eq!(progress_message(true, Some(false), false, None, true, 0.0), "Updating tour… 0%");
        assert_eq!(progress_message(true, Some(true), false, None, true, 0.4), "Cancelling…");
        // After the upload, the publish step is the same one as for any tour.
        assert_eq!(progress_message(true, Some(false), false, Some(None), true, 1.0), "Publishing…");
        assert_eq!(progress_message(true, Some(false), false, Some(Some((12, 22))), true, 1.0), "Publishing… 12 of 22 images");
        assert_eq!(progress_message(true, Some(true), false, Some(Some((12, 22))), true, 1.0), "Cancelling…");
        assert_eq!(failed_publish_message(CANCELLED, true), "Update cancelled");
        assert_eq!(failed_publish_message(CANCELLED, false), "Publishing cancelled");
        assert_eq!(failed_publish_message("Could not start the upload: that tour no longer exists.", true), "Could not start the upload: that tour no longer exists.");
        // A workspace without room is one line in the status bar; the dialog has its three parts.
        let full = "The tour was not uploaded. This tour needs 310 MB, and your workspace has 120 MB left of its 5.0 GB. Delete tours you no longer need, or ask for more room, and publish again. Tours that are already online are not affected.";
        assert_eq!(failed_publish_message(full, false), full);

        // The job itself: progress climbs through the writing and the upload, and the run
        // then carries the site's count.
        let tmp = tempfile::tempdir().unwrap();
        let s = site(Arc::new(Mutex::new(Replace::Works("ready"))), || {}, |calls| if calls == 0 { reply(202, r#"{"status":"in_progress","publish_id":"pub_0123456789abcdef01234567","done":1,"total":2}"#) } else { reply(200, LIVE) });
        let (store, _key, _info, ed) = reopened_and_changed(tmp.path(), &s);
        let run = Arc::new(Run::default());
        let seen = std::cell::RefCell::new(Vec::new());
        let counts = std::cell::RefCell::new(Vec::new());
        let done = update_job(&state_of(&ed), TITLE, &target(), &s.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), &run, &|f| {
            seen.borrow_mut().push(f);
            counts.borrow_mut().push(run.publishing());
        })
        .unwrap();
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }));
        let seen = seen.into_inner();
        assert!(seen.windows(2).all(|w| w[0] <= w[1]) && seen.last() == Some(&1.0), "{seen:?}");
        assert!(seen.iter().any(|f| *f > 0.0 && *f < PUBLISH_WRITE_SHARE) || seen.iter().any(|f| *f > PUBLISH_WRITE_SHARE && *f < 1.0), "{seen:?}");
        assert!(counts.into_inner().iter().all(|c| c.is_none()), "no count while it writes and uploads");
        assert_eq!(run.publishing(), Some(Some((1, 2))), "then the site's count");
    }

    #[test]
    fn an_update_cannot_start_while_another_export_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (_store, _key, _info, mut ed) = reopened_and_changed(tmp.path(), &s);
        let sent = s.lines().len();
        // The dialog's own check refuses with a sentence.
        assert_eq!(publish_ui::publish_blocked(true, true, &s.base, &publish_ui::TokenStatus::Saved, Some(&s.base)).map(|b| b.refusal()), Some("Wait for the running export to finish, then upload.".to_string()));
        // And should it be asked all the same, the running job stays the only one.
        let (job, go, run) = waiting_job(|| Kept::Off);
        ed.video.as_mut().unwrap().job = Some(job);
        ed.start_update(TITLE.into(), target(), Before::new_tour());
        let v = ed.video.as_ref().unwrap();
        assert!(v.job.as_ref().is_some_and(|j| !j.is_update()), "the job that was running is still the one");
        assert_ne!(ed.status_text(), Some("Updating tour… 0%"));
        assert!(!run.is_cancelled());
        assert_eq!(s.lines().len(), sent, "nothing was sent");
        go.send(()).unwrap();
    }

    #[test]
    fn update_is_only_for_the_tour_and_site_the_recording_was_saved_for() {
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (_store, key, _info, mut ed) = reopened_and_changed(tmp.path(), &s);
        // The address, exactly as the address rule writes it.
        assert!(on_this_site(&key, &s.base));
        assert!(on_this_site(&key, &format!(" {}/ ", s.base)));
        for other in ["", "https://tours.example.com", "http://127.0.0.1:1", "not an address"] {
            assert!(!on_this_site(&key, other), "{other}");
        }
        let origin = ed.video.as_ref().unwrap().origin.as_ref().unwrap();
        assert_eq!(edit_target(origin), EditTarget { tour_id: TOUR.into(), chapter_id: CH.into(), tour_title: TOUR_TITLE.into() });
        // No site address is configured in a test run, so this editor's tour is "elsewhere":
        // an update asked for all the same starts nothing.
        if !on_this_site(&key, &publish_ui::server()) {
            let sent = s.lines().len();
            ed.start_update(TITLE.into(), target(), Before::new_tour());
            assert!(!ed.exporting());
            assert_eq!(s.lines().len(), sent);
        }
        // A new recording has no tour to update.
        let fresh_state = state(&tmp.path().join("fresh"));
        let mut fresh = editor_of(&fresh_state);
        fresh.start_update(TITLE.into(), target(), Before::new_tour());
        assert!(!fresh.exporting());
    }

    #[test]
    fn closing_the_editor_does_not_cancel_a_running_update() {
        // An update, and "Try again" after one: neither is told to stop when the editor goes.
        let done = || Ok(PublishDone { finished: Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 1, outcome: Outcome::Draft }, kept: Kept::Saved { left_out: vec![], over: None } });
        let run = Arc::new(Run::default());
        let update = ExportJob::Publish(EncodeJob::spawn(move |_| done()), run.clone(), TOUR_TITLE.into(), Before::new_tour(), true);
        assert!(update.is_update());
        assert_eq!(update.cancelling(), Some(false));
        update.abandon();
        assert!(!run.is_cancelled());
        assert_eq!(update.cancelling(), Some(false));

        // In an editor: the window closes without a question, the update runs on, and the
        // project stays registered as open until the update has ended.
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (store, key, info, mut ed) = reopened_and_changed(tmp.path(), &s);
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let run = Arc::new(Run::default());
        let (state, updating, to, base, worker) = (state_of(&ed), store.clone(), target(), s.base.clone(), run.clone());
        let open = ed.video.as_ref().unwrap().origin.as_ref().unwrap().open.clone();
        let job = EncodeJob::spawn(move |progress| {
            let _ = wait.recv();
            let result = update_job(&state, TITLE, &to, &base, || Ok(TOKEN.to_string()), &Keeping::In(updating), &worker, progress);
            drop(state);
            drop(open);
            result
        });
        ed.video.as_mut().unwrap().job = Some(ExportJob::Publish(job, run.clone(), TOUR_TITLE.into(), Before::new_tour(), true));
        request_close(&mut ed);
        assert!(ed.closed, "no question: the existing one says the recording is deleted");
        drop(ed);
        assert!(!run.is_cancelled(), "the update was not cancelled behind the user's back");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(matches!(store.load(&key), Err(LoadError::AlreadyOpen)));
        assert!(!project_text(&info).contains(NOTE));
        // It runs to its end: the site is updated and published, and the saved project matches it.
        go.send(()).unwrap();
        eventually("the project is let go of once the update has ended", || !matches!(store.load(&key), Err(LoadError::AlreadyOpen)));
        assert!(s.lines().iter().any(|l| l == PUBLISH));
        assert!(project_text(&store.list()[0]).contains(NOTE));
        assert_eq!(reopened(&store, &key).doc.items().len(), 3);
    }

    /// Close an editor of a new recording while `job` runs in it; what the job says when it
    /// ends lands in the notices this returns, with the sender that lets the job end.
    fn closed_while(root: &Path, result: Result<PublishDone, String>, update: bool) -> (Notices, std::sync::mpsc::Sender<()>, PathBuf) {
        let state = state(root);
        let mut ed = editor_of(&state);
        drop(state);
        let notices = Notices::default();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let job = EncodeJob::spawn(move |_| {
            let _ = wait.recv();
            result
        });
        let v = ed.video.as_mut().unwrap();
        v.notices = notices.clone();
        v.job = Some(ExportJob::Publish(job, Arc::new(Run::default()), TOUR_TITLE.into(), Before { public: true, link: None, steps: Some(2) }, update));
        drop(ed);
        (notices, go, root.join("rec"))
    }

    #[test]
    fn an_update_that_ends_after_its_editor_was_closed_tells_the_app_how_it_ended_exactly_once() {
        let done = |outcome: Outcome| Ok(PublishDone { finished: Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 14, outcome }, kept: Kept::Saved { left_out: vec![], over: None } });
        let gone = "Could not start the upload: that tour no longer exists.";
        let cases: [(Result<PublishDone, String>, &str); 7] = [
            (done(Outcome::Public { url: Some("https://tours.example.com/t/customer-onboarding".into()) }), "Tour updated: 14 steps"),
            (done(Outcome::Draft), "Draft updated: 14 steps"),
            (done(Outcome::Held), "Update uploaded: the public page still shows the previous version"),
            (done(Outcome::NotPublic { cancelled: false, error: "Could not publish the tour: the server had an error (HTTP 500).".into() }), "Update uploaded: the public page still shows the previous version"),
            (done(Outcome::NotPublic { cancelled: true, error: String::new() }), "Update uploaded: the public page still shows the previous version"),
            // A failure is the sentence the dialog would have shown, after the tour's title.
            (Err(gone.to_string()), "\"Customer onboarding\": Could not start the upload: that tour no longer exists."),
            (Err(CANCELLED.to_string()), "\"Customer onboarding\": Update cancelled"),
        ];
        for (result, said) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let (notices, go, temporary) = closed_while(tmp.path(), result, true);
            // Nothing is said while it still runs.
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert_eq!(notices.take(), Vec::<String>::new(), "{said}");
            go.send(()).unwrap();
            // The job's thread posts, then lets go of the recording: once that is gone, it has ended.
            eventually("the job has ended", || !temporary.exists());
            assert_eq!(notices.take(), [said.to_string()]);
            assert_eq!(notices.take(), Vec::<String>::new(), "said once: {said}");
            assert!(!said.contains("https://") && !said.contains(TOKEN));
        }
    }

    #[test]
    fn only_an_update_speaks_after_its_editor_was_closed() {
        // A publish is cancelled with its editor, and whatever it ends with is not said.
        let done = || Ok(PublishDone { finished: Finished { tour_id: TOUR.into(), chapter_id: CH.into(), steps: 2, outcome: Outcome::Draft }, kept: Kept::Off });
        for result in [done(), Err(CANCELLED.to_string()), Err("Could not start the upload: the server had an error (HTTP 500).".to_string())] {
            let tmp = tempfile::tempdir().unwrap();
            let (notices, go, temporary) = closed_while(tmp.path(), result, false);
            go.send(()).unwrap();
            eventually("the job has ended", || !temporary.exists());
            assert_eq!(notices.take(), Vec::<String>::new());
        }
        // The message itself: nothing for a job that is not an update, whatever it made.
        let published = Exported::Published(done().unwrap(), TOUR_TITLE.into(), Before::new_tour(), false);
        assert_eq!(outlived_message(&Ok(published), None), None);
        assert_eq!(outlived_message(&Ok(Exported::File(PathBuf::from("a.gif"), 10)), None), None);
        assert_eq!(outlived_message(&Err("Export failed".to_string()), None), None);
        // A long title is cut as the dialog cuts it.
        let long = "A".repeat(80);
        let said = outlived_message(&Err("Could not finish the upload: no answer.".to_string()), Some(&long)).unwrap();
        assert_eq!(said, format!("\"{}…\": Could not finish the upload: no answer.", "A".repeat(39)));
        // An editor closed with no job running says nothing, and neither does one still open.
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let mut ed = editor_of(&state);
        let notices = Notices::default();
        ed.video.as_mut().unwrap().notices = notices.clone();
        drop(ed);
        assert_eq!(notices.take(), Vec::<String>::new());
    }

    #[test]
    fn an_update_takes_over_the_title_the_site_has_for_the_tour() {
        const RENAMED: &str = "Onboarding, spring edition";
        let tmp = tempfile::tempdir().unwrap();
        let s = working_site("ready");
        let (store, key, _info, ed) = reopened_and_changed(tmp.path(), &s);
        assert_eq!(store.list()[0].tour_title, TOUR_TITLE);
        // The dialog hands on the site's title. It is still this editor's tour and chapter;
        // another tour or chapter is not, whatever it is called.
        let renamed = EditTarget { tour_title: RENAMED.into(), ..target() };
        let origin = ed.video.as_ref().unwrap().origin.as_ref().unwrap();
        assert!(is_origin(&target(), origin) && is_origin(&renamed, origin));
        assert!(!is_origin(&EditTarget { chapter_id: "ch_bbbbbbbbbbbbbbbbbbbbbbbb".into(), ..target() }, origin));
        assert!(!is_origin(&EditTarget { tour_id: "tour_bbbbbbbbbbbbbbbbbbbbbbbb".into(), ..target() }, origin));
        // The update saves the project under that title: the same project, of the same tour and chapter.
        let done = update_job(&state_of(&ed), TITLE, &renamed, &s.base, || Ok(TOKEN.to_string()), &Keeping::In(store.clone()), &Arc::new(Run::default()), &|_| {}).unwrap();
        assert_eq!(done.kept, Kept::Saved { left_out: vec![], over: None });
        let list = store.list();
        assert_eq!((list.len(), list[0].tour_title.as_str(), list[0].title.as_str(), &list[0].key), (1, RENAMED, TITLE, &key));
        // Opened the next time, the editor knows the tour by the new title.
        drop(ed);
        let again = reopened(&store, &key);
        assert_eq!(edit_target(again.video.as_ref().unwrap().origin.as_ref().unwrap()), renamed);
    }

    #[test]
    fn a_project_opened_without_its_cover_logo_says_so_until_the_tour_has_a_logo_again() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = state(tmp.path());
        let logo = tmp.path().join("logo.png");
        std::fs::write(&logo, b"\x89PNG\r\n\x1a\nnot really a logo").unwrap();
        state.edit.tour_doc.logo_path = Some(logo.clone());
        let store = store_in(tmp.path());
        let key = ProjectKey { server: "https://tours.example.com".into(), tour_id: TOUR.into(), chapter_id: CH.into() };
        let snap = Snapshot { profile: "", rec: &state.rec, title: TITLE, tour_title: TOUR_TITLE, steps: 2, edit: &state.edit, events: &state.events, items: &state.items };
        let saved = store.save(&key, &snap).unwrap();
        assert_eq!(saved.left_out, Vec::<String>::new());
        drop(state);

        // Opened whole: the logo is there, and the dialog has nothing to warn of.
        let ed = reopened(&store, &key);
        let v = ed.video.as_ref().unwrap();
        assert!(v.edit.tour_doc.logo_path.is_some());
        assert!(!logo_missing(v.origin.as_ref().unwrap(), &v.edit));
        drop(ed);

        // The logo's file is taken out of the project by hand: the recording opens without it.
        let logos: Vec<PathBuf> = std::fs::read_dir(&saved.info.dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("logo-"))).collect();
        assert_eq!(logos.len(), 1);
        std::fs::remove_file(&logos[0]).unwrap();
        let mut ed = reopened(&store, &key);
        assert_eq!(ed.status_text(), Some("This recording was opened without the cover logo."));
        let v = ed.video.as_mut().unwrap();
        assert!(v.edit.tour_doc.logo_path.is_none());
        assert!(logo_missing(v.origin.as_ref().unwrap(), &v.edit), "an update would take the logo off the tour");
        // The logo is added again in Tour settings: nothing is lost by updating now.
        v.edit.tour_doc.logo_path = Some(logo);
        assert!(!logo_missing(v.origin.as_ref().unwrap(), &v.edit));
        // And taken away again by the user: that is the user's own choice, but the tour's logo
        // would go all the same, so the line is back.
        v.edit.tour_doc.logo_path = None;
        assert!(logo_missing(v.origin.as_ref().unwrap(), &v.edit));
    }

    #[test]
    fn an_editor_hands_what_outlives_it_to_the_apps_own_notices() {
        // The app reads one list, and every editor posts to it unless a test gives it another.
        let tmp = tempfile::tempdir().unwrap();
        let ed = editor_of(&state(tmp.path()));
        assert!(Arc::ptr_eq(&ed.video.as_ref().unwrap().notices.0, &background_notices().0));
    }

    /// "Publish to" for a recording: the clip job against a stand-in site.
    mod share_clip;

    /// One real pass against a platform server running on this computer; ignored unless asked for.
    mod e2e_platform;

    /// One real pass of a real recording through the folder of saved recordings, with the real
    /// ffmpeg; ignored unless asked for.
    mod e2e_recording;

    // ---- A restricted publish that did not finish ----

    fn ended(tour: &str, outcome: Outcome) -> Finished {
        Finished { tour_id: tour.into(), chapter_id: CH.into(), steps: 2, outcome }
    }

    fn not_public(error: &str) -> Outcome {
        Outcome::NotPublic { cancelled: false, error: error.into() }
    }

    const REFUSED: &str = "This site cannot send mail, so it cannot restrict who can view. Nothing was published.";

    #[test]
    fn a_tour_this_publish_created_is_deleted_again_when_the_site_did_not_take_the_restriction() {
        use crate::publish::PublishedAs;
        use crate::publish_ui::{Left, restriction_unfinished};
        let deleted = std::cell::RefCell::new(Vec::new());
        let works = |id: &str| {
            deleted.borrow_mut().push(id.to_string());
            true
        };
        let fails = |id: &str| {
            deleted.borrow_mut().push(id.to_string());
            false
        };
        let refused = PublishedAs::default();
        // Refused (any code), or not taken at the first answer: deleted, and nothing is remembered.
        let t = "tour_abababababababababab0001";
        crate::publish_ui::note_unfinished_restriction(t);
        assert_eq!(settle_restricted(true, &Target::New, &ended(t, not_public(REFUSED)), refused, &works), Some(Left::Deleted));
        assert_eq!(deleted.take(), vec![t.to_string()]);
        assert!(!restriction_unfinished(t));
        // Cancelled after the upload and before the site took the first call: the same.
        let t = "tour_abababababababababab0002";
        crate::publish_ui::note_unfinished_restriction(t);
        assert_eq!(settle_restricted(true, &Target::New, &ended(t, Outcome::NotPublic { cancelled: true, error: String::new() }), refused, &works), Some(Left::Deleted));
        assert_eq!(deleted.take(), vec![t.to_string()]);
        // The delete does not go through: the draft stays, remembered for the Tours page.
        let t = "tour_abababababababababab0003";
        crate::publish_ui::note_unfinished_restriction(t);
        assert_eq!(settle_restricted(true, &Target::New, &ended(t, not_public(REFUSED)), refused, &fails), Some(Left::StillThere));
        assert!(restriction_unfinished(t));
        deleted.take();
        // A tour that was there before is never deleted: it is remembered.
        let t = "tour_abababababababababab0004";
        crate::publish_ui::note_unfinished_restriction(t);
        assert_eq!(settle_restricted(true, &Target::Chapter(t.into()), &ended(t, not_public(REFUSED)), refused, &works), Some(Left::Draft));
        assert!(restriction_unfinished(t) && deleted.take().is_empty());
        // The site took the restriction, then the publish stopped: it stays restricted, nothing to ask.
        let t = "tour_abababababababababab0005";
        crate::publish_ui::note_unfinished_restriction(t);
        let accepted = PublishedAs { accepted: true, granted: Some(crate::publish::access::Granted::Me) };
        assert_eq!(settle_restricted(true, &Target::New, &ended(t, Outcome::NotPublic { cancelled: true, error: String::new() }), accepted, &works), Some(Left::Restricted));
        assert!(!restriction_unfinished(t) && deleted.take().is_empty());
        // Published for anyone and not taken down: said, never deleted, remembered.
        let t = "tour_abababababababababab0006";
        crate::publish_ui::note_unfinished_restriction(t);
        assert_eq!(settle_restricted(true, &Target::New, &ended(t, not_public(crate::share_ui::who::TOUR_NOT_APPLIED_PUBLIC)), refused, &works), Some(Left::Online));
        assert!(deleted.take().is_empty());
        // Published as asked: nothing to say, nothing remembered.
        let t = "tour_abababababababababab0007";
        crate::publish_ui::note_unfinished_restriction(t);
        assert_eq!(settle_restricted(true, &Target::New, &ended(t, Outcome::Public { url: None }), accepted, &works), None);
        assert!(!restriction_unfinished(t));
        // Not restricted: as before.
        assert_eq!(settle_restricted(false, &Target::New, &ended(t, not_public("Could not publish the tour: no.")), refused, &works), None);
        assert!(deleted.take().is_empty());
    }

    /// A site that takes the upload of tour `tour` and answers its first publish call with `first`.
    fn refusing_site(tour: &'static str, first: Answer) -> Stub {
        let first = std::sync::Mutex::new(Some(first));
        stub(move |req, seen| {
            let l = req.line.as_str();
            if l == format!("POST /api/v1/tours/{tour}/publish") {
                return first.lock().unwrap().take().unwrap_or_else(|| reply(500, r#"{"error":"internal"}"#));
            }
            if l == format!("DELETE /api/v1/tours/{tour}") {
                return reply(200, r#"{"deleted":true}"#);
            }
            if l.ends_with("/commit") {
                return reply(201, &format!(r#"{{"tour_id":"{tour}","chapter_id":"{CH}","steps":2}}"#));
            }
            platform(req, seen)
        })
    }

    #[test]
    fn a_refused_restricted_publish_of_a_new_tour_leaves_nothing_on_the_site() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let limit = Limit { warn_gb: 0, totals: &Store::totals };
        for (n, first) in [(1, r#"{"error":"cannot_restrict"}"#), (2, r#"{"error":"bad_viewer","detail":"viewers[0] is not an email address; nothing was changed"}"#), (3, r#"{"error":"access_already_set"}"#), (4, r#"{"error":"bad_request"}"#)] {
            let tour: &'static str = Box::leak(format!("tour_cdcdcdcdcdcdcdcdcdcd000{n}").into_boxed_str());
            let status = if first.contains("access_already_set") || first.contains("cannot_restrict") { 409 } else { 400 };
            let s = refusing_site(tour, reply(status, first));
            let run = Arc::new(Run::default());
            let base = s.base.clone();
            let delete = move |token: &str, id: &str| crate::publish::delete_tour(&base, token, id, |_| {}).is_ok();
            let done = publish_job_full(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &Access::Me, &Keeping::Off, &run, &|_| {}, &store_save, &limit, &delete).unwrap();
            assert!(matches!(done.finished.outcome, Outcome::NotPublic { cancelled: false, .. }), "{first}");
            assert_eq!(run.viewing().left, Some(crate::publish_ui::Left::Deleted), "{first}");
            assert!(s.lines().contains(&format!("DELETE /api/v1/tours/{tour}")), "{first}: {:?}", s.lines());
            assert!(!crate::publish_ui::restriction_unfinished(tour));
        }
        // The delete is refused: the draft is still there, and the Tours page will ask.
        let tour = "tour_cdcdcdcdcdcdcdcdcdcd0009";
        let s = refusing_site(tour, reply(409, r#"{"error":"cannot_restrict"}"#));
        let run = Arc::new(Run::default());
        let done = publish_job_full(&state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), &Target::New, true, &Access::Me, &Keeping::Off, &run, &|_| {}, &store_save, &limit, &|_, _| false).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::NotPublic { .. }));
        assert_eq!(run.viewing().left, Some(crate::publish_ui::Left::StillThere));
        assert!(crate::publish_ui::restriction_unfinished(tour));
    }

    /// A site that takes the upload of `tour` (its commit answered by `commit` when given),
    /// answers the first publish call with `first`, deletes on request, and calls `on_commit`
    /// when the commit comes.
    fn site_with(tour: &'static str, first: Answer, commit: Option<Answer>, on_commit: impl Fn() + Send + 'static) -> Stub {
        let first = std::sync::Mutex::new(Some(first));
        let commit = std::sync::Mutex::new(commit);
        stub(move |req, seen| {
            let l = req.line.as_str();
            if l == format!("POST /api/v1/tours/{tour}/publish") {
                return first.lock().unwrap().take().unwrap_or_else(|| reply(500, r#"{"error":"internal"}"#));
            }
            if l == format!("DELETE /api/v1/tours/{tour}") {
                return reply(200, r#"{"deleted":true}"#);
            }
            if l.ends_with("/commit") {
                on_commit();
                if let Some(answer) = commit.lock().unwrap().as_ref() {
                    return match answer {
                        Answer::Drop => Answer::Drop,
                        Answer::Hang => Answer::Hang,
                        Answer::Reply(s, b, h) => Answer::Reply(*s, b.clone(), h.clone()),
                    };
                }
                return reply(201, &format!(r#"{{"tour_id":"{tour}","chapter_id":"{CH}","steps":2}}"#));
            }
            platform(req, seen)
        })
    }

    fn job_at(s: &Stub, state: &PublishState, target: &Target, access: &Access, run: &Arc<Run>) -> Result<PublishDone, String> {
        let limit = Limit { warn_gb: 0, totals: &Store::totals };
        let base = s.base.clone();
        let delete = move |token: &str, id: &str| crate::publish::delete_tour(&base, token, id, |_| {}).is_ok();
        publish_job_full(state, TITLE, TOUR_TITLE, &s.base, || Ok(TOKEN.to_string()), target, true, access, &Keeping::Off, run, &|_| {}, &store_save, &limit, &delete)
    }

    #[test]
    fn no_answer_to_the_first_publish_call_is_taken_as_a_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        // A new tour: deleted again.
        let tour = "tour_efefefefefefefefefef0001";
        let s = site_with(tour, Answer::Drop, None, || {});
        let run = Arc::new(Run::default());
        let done = job_at(&s, &state, &Target::New, &Access::Me, &run).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::NotPublic { cancelled: false, .. }));
        assert_eq!(run.viewing().left, Some(crate::publish_ui::Left::Deleted));
        assert!(s.lines().contains(&format!("DELETE /api/v1/tours/{tour}")));
        assert!(!crate::publish_ui::restriction_unfinished(tour));
        // A draft that was there: remembered, never deleted.
        let tour = "tour_efefefefefefefefefef0002";
        let s = site_with(tour, Answer::Drop, None, || {});
        let run = Arc::new(Run::default());
        job_at(&s, &state, &Target::Chapter(tour.into()), &Access::Me, &run).unwrap();
        assert_eq!(run.viewing().left, Some(crate::publish_ui::Left::Draft));
        assert!(!s.lines().iter().any(|l| l.starts_with("DELETE")));
        assert!(crate::publish_ui::restriction_unfinished(tour));
        crate::publish_ui::settle_restriction(tour);
    }

    #[test]
    fn a_cancel_after_the_upload_and_before_the_first_publish_call_deletes_the_new_tour() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let tour = "tour_efefefefefefefefefef0003";
        let run = Arc::new(Run::default());
        let cancel = run.clone();
        let s = site_with(tour, reply(500, "{}"), None, move || cancel.cancel());
        let done = job_at(&s, &state, &Target::New, &Access::Me, &run).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::NotPublic { cancelled: true, .. }), "{:?}", done.finished.outcome);
        assert!(!s.lines().iter().any(|l| l.ends_with("/publish")), "no publish call was sent");
        assert_eq!(run.viewing().left, Some(crate::publish_ui::Left::Deleted));
        assert!(s.lines().contains(&format!("DELETE /api/v1/tours/{tour}")));
    }

    #[test]
    fn a_lost_commit_answer_during_a_restricted_publish_warns_about_a_draft_that_may_appear() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let tour = "tour_efefefefefefefefefef0004";
        let s = site_with(tour, reply(500, "{}"), Some(Answer::Drop), || {});
        let e = job_at(&s, &state, &Target::New, &Access::Me, &Arc::new(Run::default())).unwrap_err();
        assert!(e.starts_with("Could not finish the upload") && e.ends_with(" If a draft appears on the Tours page, publishing it there makes it visible to anyone with the link."), "{e}");
        // Without a restriction the error is as before.
        let s = site_with(tour, reply(500, "{}"), Some(Answer::Drop), || {});
        let plain = job_at(&s, &state, &Target::New, &Access::Public, &Arc::new(Run::default())).unwrap_err();
        assert!(plain.starts_with("Could not finish the upload") && plain.ends_with("Check the tour list before uploading again."), "{plain}");
        assert_eq!(e, format!("{plain} {}", crate::share_ui::who::DRAFT_MAY_APPEAR));
    }

    #[test]
    fn a_tour_published_for_anyone_is_no_longer_remembered() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let tour = "tour_efefefefefefefefefef0005";
        crate::publish_ui::note_unfinished_restriction(tour);
        let s = site_with(tour, reply(200, LIVE), None, || {});
        let done = job_at(&s, &state, &Target::Chapter(tour.into()), &Access::Public, &Arc::new(Run::default())).unwrap();
        assert!(matches!(done.finished.outcome, Outcome::Public { .. }));
        assert!(!crate::publish_ui::restriction_unfinished(tour));
    }

    // ---- A picture file dropped on a recording ----

    /// A picture dropped on the recording editor is placed wherever a picture may be pasted: in
    /// the Timeline view (shown from the current frame for a while) and in the Tour view on a step
    /// (pinned to that step). In the Tour view without a step to pin to, a picture dragged there
    /// is refused with that reason, and a drop places nothing.
    #[test]
    fn a_picture_is_placed_on_a_recording_wherever_one_may_be_pasted_and_refused_where_not() {
        use crate::editor_ui::tests::{drop_on, file_frame, frames_until};
        use crate::editor_ui::dialog_checks;
        let tmp = tempfile::tempdir().unwrap();
        let pic = crate::place_picture::tests::picture_file(tmp.path(), "logo.png", 20, 10, [255, 0, 0, 255]);
        let ctx = crate::theme::test_context();
        let image_items = |ed: &Editor| ed.doc.items().iter().filter(|it| matches!(it.shape, Shape::Image { .. })).cloned().collect::<Vec<_>>();
        // The Timeline view.
        let mut ed = video_editor_with(&tmp.path().join("timeline"), Default::default());
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        assert!(ed.can_place_picture());
        drop_on(&mut ed, &ctx, &pic, pt(60.0, 50.0));
        let placed = image_items(&ed);
        assert_eq!(placed.len(), 1, "{placed:?}");
        assert!(placed[0].frames.is_some() && placed[0].pin.is_none(), "{:?}", placed[0]);
        assert_eq!(ed.selected, Some(placed[0].id));
        // The Tour view on a step: pinned to it.
        let mut ed = video_editor_with(&tmp.path().join("tour"), Default::default());
        ed.set_view(VideoView::Tour);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        let src = ed.tour_src().expect("a step is shown");
        drop_on(&mut ed, &ctx, &pic, pt(60.0, 50.0));
        let placed = image_items(&ed);
        assert_eq!(placed.len(), 1, "{placed:?}");
        assert_eq!(placed[0].pin, Some(src as u32));
        // The Tour view of a recording with no clicks, so no step to pin to: refused.
        let s = Settings::default();
        let mut ed = Editor::new_video(recorded(&tmp.path().join("no-steps")), Vec::new(), Vec::new(), TITLE.into(), Default::default(), s.dest_context(TITLE), OverlayStyle::default(), s.editor.clone()).unwrap();
        ed.set_view(VideoView::Tour);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        dialog_checks::frame(&mut ed, &ctx, vec![]);
        assert!(!ed.can_place_picture());
        let out = file_frame(&mut ed, &ctx, std::slice::from_ref(&pic), &[], vec![]);
        let texts: Vec<String> = crate::share_ui::tests::shapes_of(&out).into_iter().filter_map(|s| if let egui::Shape::Text(t) = s { Some(t.galley.text().to_string()) } else { None }).collect();
        assert!(texts.iter().any(|t| t == crate::place_picture::CANNOT_PLACE_NOW), "{texts:?}");
        assert!(!texts.iter().any(|t| t == crate::open_image::ONLY_PICTURES || t == crate::place_picture::PLACE_HERE), "{texts:?}");
        file_frame(&mut ed, &ctx, &[], std::slice::from_ref(&pic), vec![]);
        frames_until(&mut ed, &ctx, |e| !e.holds_update());
        assert!(image_items(&ed).is_empty(), "nothing was placed");
        // Asked for directly, nothing is read either.
        ed.place_picture_file(pic.clone(), pt(10.0, 10.0));
        assert!(!ed.holds_update());
        // Nor does the Picture tool ask for a file there.
        mythicshot_destinations::stubs::take();
        ed.picture_tool();
        assert!(mythicshot_destinations::stubs::take().is_empty());
    }
}
