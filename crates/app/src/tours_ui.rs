//! The Tours page of the main window: the tours on the connected site, and what can be done
//! with each: copy its link, open it, edit its saved recording, publish, unpublish, rename and
//! delete.
//!
//! The page is a [`Model`] (what is shown and what may happen next, with no drawing and no
//! network in it) and a thin layer around it that draws it and runs its jobs. Every request
//! runs on a background thread, which reports back into the model; the page only reads it,
//! so it never waits. Closing the window or switching page does not stop a job: it holds
//! the model itself and finishes on its own.
//!
//! Rules the model keeps:
//! - Everything is addressed by tour id, never by a row's position. A delete is sent only
//!   for the tour whose confirmation is showing, and a refresh that changes the list under
//!   an open confirmation closes it instead of leaving it pointing somewhere new.
//! - One job per tour at a time. While it runs, that tour's buttons are gone, so a second
//!   click has nothing to press; the other tours stay usable.
//! - A delete that removed the tour but not all of its files is written down (site, tour id
//!   and title, in the settings) until the site says nothing is left: the tour is gone from
//!   the list by then, so that note is the only way back to it. It has its own block on the
//!   page, which no refresh, other message, page switch or restart takes away.
//! - A job runs against the site the list came from. When the site or the token changes
//!   (Forget included), the list is dropped and a job still running only finishes: its
//!   result is not shown on a page that now belongs to another site.
//! - The recordings saved on this computer are read with the list, off the window's thread, and
//!   again when the store says something was saved or removed; never while drawing. A saved
//!   recording belongs to a tour by the site's exact address and the tour's id. It is removed
//!   only after the site has said its tour is gone, and "no such tour" counts as that only for
//!   a tour this workspace's own list held: the site says the same of another workspace's tour.
//! - A recording kept as a video is expanded for its editor on a thread of the app's, never
//!   here: the row shows how far it is, with Cancel, and one recording opens at a time. What
//!   the row shows is read from the one record of that work, so it is there whenever the tour
//!   is listed for the site it was asked on; a list without the tour stops nothing.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local, TimeZone};
use eframe::egui::{self, Align, CornerRadius, Layout, RichText, Sense, Stroke, vec2};
use mythicshot_recording::VideoCodec;

use crate::theme::{self, Strong as _};
use crate::main_window::{Page, PageOut};
use crate::project::{self, Expand, FfmpegState, Form, LoadError, Loaded, Problem, ProjectInfo, ProjectKey, Store};
use crate::publish::{self, Images, RemoteTour};
use crate::publish_ui::{self, TITLE_MAX, TokenStatus};
use crate::settings::UnfinishedDelete;


pub const NOT_CONNECTED: &str = "Mythicshot is not connected to a tour site yet.";
pub const OPEN_WORKSPACE: &str = "Open Workspace";
pub const CHECKING: &str = "Checking for a saved token…";
pub const LOADING: &str = "Loading the tours…";
pub const EMPTY: &str = "This workspace has no tours yet. Record a tour and publish it to see it here.";
pub const RECORD_A_TOUR: &str = "Record a tour";
pub const RENAME_NOTE: &str = "The link stays the same after a rename.";
pub const LINK_COPIED: &str = "Link copied";
/// On the detail line of a public tour whose live page is behind its draft.
pub const CHANGES_WAITING: &str = "changes waiting to be published";
pub const LIST_CHANGED: &str = "The tour list changed, so nothing was deleted. Check the list and choose Delete again.";

/// Edit on a tour with no recording saved on this computer for it.
pub const NOT_SAVED: &str = "This tour's recording is not saved on this computer, so it can't be edited here.";
/// Above the saved chapters of a tour with more than one chapter. The wording is proposed and waits for the owner's approval.
pub const WHICH_CHAPTER: &str = "Which chapter do you want to edit?";
/// Edit on a saved recording that cannot be read. The wording is proposed and waits for the owner's approval.
pub const SAVED_DAMAGED: &str = "This tour's saved recording is damaged, so it can't be opened here. The tour on the site is not affected.";
/// Edit on a saved recording a newer Mythicshot wrote. The wording is proposed and waits for the owner's approval.
pub const SAVED_NEWER: &str = "This tour's recording was saved by a newer version of Mythicshot. Update Mythicshot to edit it.";
/// Edit on a saved recording whose frames file is gone or is not the one it was saved with. The
/// wording is proposed and waits for the owner's approval.
pub const SAVED_FRAMES_MISSING: &str = "This tour's saved recording is missing its frames, so it can't be opened here. The tour on the site is not affected.";
/// Edit on a saved recording an editor already has open. The wording is proposed and waits for the owner's approval.
pub const ALREADY_OPEN: &str = "This tour's recording is already open in an editor.";
/// After a delete whose saved recording could not be taken away yet. The wording is proposed and
/// waits for the owner's approval.
pub const SAVED_NOT_REMOVED: &str = "The saved recording could not be removed yet. It is removed the next time Mythicshot starts.";
/// Edit on a recording that is kept as a video, on a computer where no ffmpeg that can read it was found.
pub const NEEDS_FFMPEG: &str = "This tour's recording is stored as video, and ffmpeg was not found on this computer. Install ffmpeg and start Mythicshot again to edit it. Nothing was deleted.";
/// Edit on a recording whose video could not be read back into its frames.
pub const VIDEO_DAMAGED: &str = "This tour's recording could not be read back from its video, so it can't be opened here. The recording is left as it is, and the tour on the site is not affected.";
/// Edit on a recording kept as a video, with too little room on the disk for its frames.
pub const NO_ROOM: &str = "There is not enough room on this computer to open this tour's recording. Free some space and choose Edit again.";
/// Edit on a recording kept as a video while another one is being opened.
pub const OPENING_ANOTHER: &str = "Another recording is being opened. Wait for it to finish, or cancel it.";
/// A saved chapter without a title, in the chapter question. The wording is proposed and waits for the owner's approval.
const UNTITLED_CHAPTER: &str = "Untitled chapter";

pub const FINISH_DELETING: &str = "Finish deleting";
pub const JOB_BROKE: &str = "That did not finish because of an unexpected error in Mythicshot. The list is read again to show where the tour stands.";
/// A delete that was not sent, because its note could not be written to the settings file.
pub const NOTE_NOT_SAVED: &str = "Could not save a note about this delete, so nothing was deleted. Check that the settings file can be written, then try again.";
const LOAD_BROKE: &str = "Could not list the tours: an unexpected error in Mythicshot stopped it.";
/// Most unfinished deletes kept; the oldest go first.
const MAX_UNFINISHED: usize = 200;

/// How long "Link copied" shows.
const COPIED_FOR: Duration = Duration::from_secs(2);
/// A list older than this is read again when the page is shown and nothing is being edited.
const STALE_AFTER: Duration = Duration::from_secs(60);

// ---- What a row says ----

fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// "Public" or "Draft".
pub fn badge(t: &RemoteTour) -> &'static str {
    if t.is_public() { "Public" } else { "Draft" }
}

/// A day as the rows show it, "18 Nov", with the year when it is not this one. None for a time that cannot be a date.
pub fn day_label<Tz: TimeZone>(ms: i64, now: &DateTime<Tz>) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let day = DateTime::from_timestamp_millis(ms)?.with_timezone(&now.timezone());
    Some(if day.year() == now.year() { day.format("%-d %b").to_string() } else { day.format("%-d %b %Y").to_string() })
}

/// The saved recordings that can be opened: those the list's check found nothing wrong with.
fn usable(saved: &[ProjectInfo]) -> impl Iterator<Item = &ProjectInfo> {
    saved.iter().filter(|p| p.problem.is_none())
}

/// What the detail line says about the recordings saved on this computer for a tour:
/// "recording saved on this computer (12.4 MB)", the size written as Preferences writes it
/// ([`publish_ui::size_text`]). A recording with a problem is not counted, so the line never
/// promises one that cannot be opened. `converting` is the folder of the project that is being
/// stored as video, when one is: a tour that has it among its recordings says
/// "(12.4 MB, compressing…)".
pub fn saved_note(saved: &[ProjectInfo], converting: Option<&Path>) -> Option<String> {
    let size = publish_ui::size_text(usable(saved).map(|p| p.bytes).sum());
    let compressing = if converting.is_some_and(|dir| usable(saved).any(|p| p.dir == dir)) { ", compressing…" } else { "" };
    match usable(saved).count() {
        0 => None,
        1 => Some(format!("recording saved on this computer ({size}{compressing})")),
        n => Some(format!("{n} recordings saved on this computer ({size}{compressing})")),
    }
}

/// What Edit says for a saved recording the list's check found a problem with.
pub fn problem_sentence(problem: Problem) -> &'static str {
    match problem {
        Problem::Damaged => SAVED_DAMAGED,
        Problem::Newer => SAVED_NEWER,
        Problem::FramesMissing => SAVED_FRAMES_MISSING,
    }
}

/// What Edit says when the saved recording could not be opened after all. The store's own
/// reason is not shown: it can hold the system's words. None after a cancel: that is what was
/// asked for, and nothing is said.
pub fn load_sentence(error: &LoadError) -> Option<&'static str> {
    match error {
        LoadError::NotSaved => Some(NOT_SAVED),
        LoadError::Damaged(_) => Some(SAVED_DAMAGED),
        LoadError::Newer => Some(SAVED_NEWER),
        LoadError::AlreadyOpen => Some(ALREADY_OPEN),
        LoadError::NeedsFfmpeg => Some(NEEDS_FFMPEG),
        LoadError::VideoDamaged => Some(VIDEO_DAMAGED),
        LoadError::NoRoom => Some(NO_ROOM),
        LoadError::Cancelled => None,
    }
}

/// The muted line under a title: "2 chapters · 22 steps · published 18 Nov", or for a draft
/// "… · updated 18 Nov" (the site's date for a draft moves with every change, a rename
/// included). A site that gives no date (an older one) gets the line without it. A public tour
/// whose live page is behind its draft says so next: "… · changes waiting to be published".
/// `saved` is what is saved on this computer for this tour, and `converting` the folder of the
/// project that is being stored as video; see [`saved_note`].
pub fn detail_line<Tz: TimeZone>(t: &RemoteTour, saved: &[ProjectInfo], converting: Option<&Path>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut line = format!("{} · {}", count(t.chapters.into(), "chapter", "chapters"), count(t.steps.into(), "step", "steps"));
    let dated = if t.is_public() { t.published_at.map(|ms| ("published", ms)) } else { t.updated_at.map(|ms| ("updated", ms)) };
    if let Some((word, day)) = dated.and_then(|(word, ms)| Some((word, day_label(ms, now)?))) {
        line.push_str(&format!(" · {word} {day}"));
    }
    if t.changes_waiting() {
        line.push_str(&format!(" · {CHANGES_WAITING}"));
    }
    if let Some(note) = saved_note(saved, converting) {
        line.push_str(&format!(" · {note}"));
    }
    line
}

/// A button on a tour's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowButton {
    CopyLink,
    Open,
    Edit,
    /// On a public tour whose live page is behind its draft: publish what is waiting.
    PublishChanges,
    Publish,
    Rename,
    Unpublish,
    Delete,
}

impl RowButton {
    pub fn label(self) -> &'static str {
        match self {
            RowButton::CopyLink => "Copy link",
            RowButton::Open => "Open",
            RowButton::Edit => "Edit",
            RowButton::PublishChanges => "Publish changes",
            RowButton::Publish => "Publish",
            RowButton::Rename => "Rename",
            RowButton::Unpublish => "Unpublish",
            RowButton::Delete => "Delete",
        }
    }
}

/// The buttons a tour's row shows, in order. A public tour whose link the site did not give
/// (an older site) has no Copy link and no Open: there is nothing true to copy. Every row has
/// Edit, after Open or first among the buttons that manage the tour; whether there is a
/// recording to edit is said when it is pressed. A public tour whose live page is behind its
/// draft ([`RemoteTour::changes_waiting`]) has Publish changes directly after Edit; a draft
/// keeps its own Publish.
pub fn row_buttons(t: &RemoteTour) -> Vec<RowButton> {
    use RowButton::*;
    let mut buttons = match (t.is_public(), t.link.is_some()) {
        (true, true) => vec![CopyLink, Open, Edit, Rename, Unpublish, Delete],
        (true, false) => vec![Edit, Rename, Unpublish, Delete],
        (false, _) => vec![Publish, Edit, Rename, Delete],
    };
    if t.changes_waiting()
        && let Some(edit) = buttons.iter().position(|b| *b == Edit)
    {
        buttons.insert(edit + 1, PublishChanges);
    }
    buttons
}

/// The question the delete confirmation asks. `saved`: how many recordings of this tour are
/// saved on this computer, one per saved chapter and those with a problem included: they all go
/// with the tour. The sentence for several is proposed and waits for the owner's approval.
pub fn delete_question(t: &RemoteTour, saved: usize) -> String {
    let saved = match saved {
        0 => "",
        1 => " The recording saved on this computer is deleted too.",
        _ => " The recordings saved on this computer are deleted too.",
    };
    format!("Delete \"{}\"? Its {} and images are removed from the site.{saved} This can't be undone.", publish_ui::short_title(&t.title, 60), count(t.steps.into(), "step", "steps"))
}

/// What a saved chapter's button says in the chapter question.
pub fn chapter_label(p: &ProjectInfo) -> String {
    if p.title.trim().is_empty() { UNTITLED_CHAPTER.to_string() } else { publish_ui::short_title(p.title.trim(), 40) }
}

/// The line under the list: "2 tours in this workspace".
pub fn footer(tours: usize) -> String {
    format!("{} in this workspace", count(tours as u64, "tour", "tours"))
}

/// What the block for an unfinished delete says. True also when the answer was lost and the
/// site in fact removed everything, or nothing: "Finish deleting" finds out either way.
pub fn unfinished_sentence(title: &str) -> String {
    format!("Deleting \"{}\" did not finish. Some of its files may still be on the site.", publish_ui::short_title(title, 60))
}

/// What a job on a tour is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Publishing(Option<Images>),
    Unpublishing,
    Renaming,
    Deleting(Option<Images>),
    /// A recording kept as a video is being expanded for an editor: frames done, and their number.
    Opening(Option<(u32, u32)>),
}

/// "Publishing… 12 of 22 images", "Deleting… 250 of 601 files", "Opening the recording… 40%".
pub fn work_message(work: Work) -> String {
    match work {
        Work::Publishing(images) => publish_ui::publishing_message(images),
        Work::Unpublishing => "Unpublishing…".to_string(),
        Work::Renaming => "Saving the new name…".to_string(),
        Work::Deleting(None) => "Deleting…".to_string(),
        Work::Deleting(Some((done, 1))) => format!("Deleting… {done} of 1 file"),
        Work::Deleting(Some((done, total))) => format!("Deleting… {done} of {total} files"),
        Work::Opening(Some((done, total))) if total > 0 => format!("Opening the recording… {}%", u64::from(done.min(total)) * 100 / u64::from(total)),
        Work::Opening(_) => "Opening the recording…".to_string(),
    }
}

// ---- The connection ----

/// What the page is told about the connection each frame: the address and token state as
/// saved, and a number that changes whenever the token is saved, replaced or forgotten.
#[derive(Debug, Clone, PartialEq)]
pub struct Conn {
    pub server: String,
    pub token: TokenStatus,
    pub epoch: u64,
    /// The address the saved token belongs to. It is sent to no other.
    pub bound: Option<String>,
}

impl Conn {
    pub fn current() -> Conn {
        Conn { server: publish_ui::server(), token: publish_ui::token_status(), epoch: publish_ui::token_epoch(), bound: publish_ui::token_site() }
    }

    /// Why the saved token may not go to the configured address, when it may not.
    pub fn binding_problem(&self) -> Option<String> {
        if self.token != TokenStatus::Saved {
            return None;
        }
        publish_ui::binding_problem(self.bound.as_deref(), &self.server)
    }

    /// The site to talk to: only with a usable address and a token saved for that address.
    pub fn site(&self) -> Option<Site> {
        if self.token != TokenStatus::Saved || self.binding_problem().is_some() {
            return None;
        }
        Some(Site { server: publish::check_server(&self.server).ok()?, epoch: self.epoch })
    }
}

/// A site and token the list was read with. A list or a job belongs to exactly one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Site {
    pub server: String,
    pub epoch: u64,
}

// ---- The model ----

/// What is being edited on the page. At most one thing at a time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Edit {
    #[default]
    None,
    /// The title of this tour is a text field.
    Renaming { id: String, text: String },
    /// The delete confirmation of this tour is showing.
    ConfirmDelete { id: String },
    /// The question which saved chapter of this tour to edit is showing.
    ChoosingChapter { id: String },
    /// The question whether to publish for anyone a tour whose restriction was not applied.
    ConfirmPublish { id: String },
}

impl Edit {
    /// The tour the edit is on.
    fn tour_id(&self) -> Option<&str> {
        match self {
            Edit::None => None,
            Edit::Renaming { id, .. } | Edit::ConfirmDelete { id } | Edit::ChoosingChapter { id } | Edit::ConfirmPublish { id } => Some(id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Publish,
    Unpublish,
    Rename(String),
    Delete,
}

/// A job the model has agreed to: run it on a background thread, then call [`Model::finish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub site: Site,
    pub id: String,
    /// The tour's title when the job started, to name it should its delete not finish.
    pub title: String,
    pub job: Job,
    /// The list read from this site with this token held the tour when the job was asked for.
    /// False for "Finish deleting", which is asked for a tour the list no longer holds.
    pub listed: bool,
}

/// A job that worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    Published(Option<String>),
    Unpublished,
    Renamed(String),
    Deleted,
    /// Deleted on the site, with a saved recording of it that could not be removed yet.
    DeletedSavedLeft,
}

/// A job that did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The tour is no longer on the site.
    Gone,
    Message(String),
    /// A delete that removed the tour but not all of its files.
    DeleteUnfinished(String),
}

/// A line above the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// What the page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// No usable address or token; the text says which.
    NotConnected(String),
    Checking,
    Loading,
    Failed(String),
    Empty,
    List,
}

#[derive(Debug, Default)]
enum Listing {
    #[default]
    NotLoaded,
    Loaded,
    Failed(String),
}

/// A recording kept as a video that is being expanded for an editor.
#[derive(Debug)]
struct Opening {
    key: ProjectKey,
    /// Stops the work; set by Cancel on the row, and by the app when it closes.
    cancel: Arc<AtomicBool>,
    /// Frames done and their number, once the work has reported.
    progress: Option<(u32, u32)>,
}

#[derive(Debug, Default)]
pub struct Model {
    site: Option<Site>,
    listing: Listing,
    /// The number of the list request that is running.
    loading: Option<u64>,
    generation: u64,
    loaded_at: Option<Instant>,
    /// Something happened that the list on screen may not show yet.
    refresh_wanted: bool,
    tours: Vec<RemoteTour>,
    edit: Edit,
    working: HashMap<String, Work>,
    errors: HashMap<String, String>,
    notice: Option<Notice>,
    copied: Option<(String, Instant)>,
    /// The recordings saved on this computer for this site, by tour id, those with a problem
    /// included. Filled by a background thread; drawing only reads it.
    saved: HashMap<String, Vec<ProjectInfo>>,
    /// The store's counter ([`project::epoch`]) when `saved` was read. None: not read yet.
    saved_epoch: Option<u64>,
    /// `saved` is being read again because the store changed.
    saved_reading: bool,
    /// The folder of the project that was being stored as video when `saved` was read
    /// ([`project::converting_now`]). Read with `saved`, never while drawing.
    converting: Option<PathBuf>,
    /// The one recording kept as a video that is being expanded for an editor. It stays until
    /// the work has ended, whatever the page shows meanwhile; the tour's row reads its state
    /// from it ([`Model::work`]).
    opening: Option<Opening>,
    /// The search for ffmpeg ended without one that can read a recording kept as a video.
    no_ffmpeg: bool,
}

impl Model {
    #[cfg(test)]
    pub fn tours(&self) -> &[RemoteTour] {
        &self.tours
    }

    #[cfg(test)]
    pub fn edit(&self) -> &Edit {
        &self.edit
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    pub fn work(&self, id: &str) -> Option<Work> {
        self.working.get(id).copied().or_else(|| self.opening_on(id).map(|opening| Work::Opening(opening.progress)))
    }

    /// The recording that is being opened for the tour `id`, when its row shows that: the tour
    /// is listed for the site the recording belongs to, and Cancel was not pressed.
    fn opening_on(&self, id: &str) -> Option<&Opening> {
        let opening = self.opening.as_ref().filter(|opening| opening.key.tour_id == id && !opening.cancel.load(Ordering::SeqCst))?;
        let listed = self.site.as_ref().is_some_and(|site| site.server == opening.key.server) && self.tour(id).is_some();
        listed.then_some(opening)
    }

    pub fn error(&self, id: &str) -> Option<&str> {
        self.errors.get(id).map(String::as_str)
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    fn tour(&self, id: &str) -> Option<&RemoteTour> {
        self.tours.iter().find(|t| t.id == id)
    }

    /// The recordings saved on this computer for this tour, one per saved chapter, those with
    /// a problem included.
    pub fn saved(&self, id: &str) -> &[ProjectInfo] {
        self.saved.get(id).map_or(&[], Vec::as_slice)
    }

    fn tour_mut(&mut self, id: &str) -> Option<&mut RemoteTour> {
        self.tours.iter_mut().find(|t| t.id == id)
    }

    /// Whether an action may start on this tour: it is in the list and nothing runs on it.
    fn can_act(&self, id: &str) -> bool {
        self.site.is_some() && self.tour(id).is_some() && self.work(id).is_none()
    }

    /// Tell the model which site the page belongs to now. A change (another address, a token
    /// saved or forgotten) drops the list and whatever was being edited. Jobs still running
    /// are left alone; their results are for the old site and are not shown.
    pub fn set_site(&mut self, site: Option<Site>) {
        if self.site == site {
            return;
        }
        self.site = site;
        self.listing = Listing::NotLoaded;
        self.loading = None;
        self.generation += 1;
        self.loaded_at = None;
        self.refresh_wanted = false;
        self.tours.clear();
        self.edit = Edit::None;
        self.errors.clear();
        self.notice = None;
        self.copied = None;
        self.saved.clear();
        self.saved_epoch = None;
        self.saved_reading = false;
        self.converting = None;
    }

    /// Whether the saved recordings should be read again: the store's counter is not the one
    /// they were read at. Not while the list is being read, which reads them too, and not
    /// without a list to show them on.
    pub fn wants_saved(&self, epoch: u64) -> bool {
        self.site.is_some() && !self.saved_reading && self.loading.is_none() && matches!(self.listing, Listing::Loaded) && self.saved_epoch != Some(epoch)
    }

    /// Start reading the saved recordings again: the site to read them for.
    pub fn begin_saved(&mut self) -> Option<Site> {
        let site = self.site.clone()?;
        if self.saved_reading {
            return None;
        }
        self.saved_reading = true;
        Some(site)
    }

    /// The read [`Model::begin_saved`] started has ended.
    pub fn finish_saved(&mut self, site: &Site, epoch: u64, list: Vec<ProjectInfo>, converting: Option<PathBuf>) {
        self.saved_reading = false;
        self.apply_saved(site, epoch, list, converting);
    }

    /// What is saved on this computer for `site`, read while the store's counter was `epoch`.
    /// Dropped when the page belongs to another site by now, or already holds a later read.
    /// Only a recording saved for exactly this site's address is kept. A chapter question
    /// with no chapter left to offer closes. `converting` is the folder of the project that was
    /// being stored as video when the list was read; it is kept with the list.
    pub fn apply_saved(&mut self, site: &Site, epoch: u64, list: Vec<ProjectInfo>, converting: Option<PathBuf>) {
        if self.site.as_ref() != Some(site) || self.saved_epoch.is_some_and(|have| have > epoch) {
            return;
        }
        self.converting = converting;
        let mut saved: HashMap<String, Vec<ProjectInfo>> = HashMap::new();
        for p in list.into_iter().filter(|p| p.key.server == site.server) {
            saved.entry(p.key.tour_id.clone()).or_default().push(p);
        }
        self.saved = saved;
        self.saved_epoch = Some(epoch);
        if matches!(&self.edit, Edit::ChoosingChapter { id } if usable(self.saved(id)).next().is_none()) {
            self.edit = Edit::None;
        }
    }

    /// What to show. `not_connected` is why there is no site, when there is none.
    pub fn view(&self, conn: &Conn) -> View {
        if self.site.is_none() {
            return match &conn.token {
                TokenStatus::Checking => View::Checking,
                TokenStatus::Unavailable(e) => View::NotConnected(e.clone()),
                _ => View::NotConnected(match conn.binding_problem() {
                    Some(problem) => format!("{problem} {}", publish_ui::FIX_ON_WORKSPACE),
                    None => NOT_CONNECTED.to_string(),
                }),
            };
        }
        match &self.listing {
            Listing::NotLoaded => View::Loading,
            // A token the site refused (revoked there): not connected, with the way to the
            // Workspace page, instead of a Retry that would be refused again.
            Listing::Failed(e) if e == publish::REJECTED_TOKEN => View::NotConnected(publish_ui::refused_reason()),
            Listing::Failed(e) => View::Failed(e.clone()),
            Listing::Loaded if self.tours.is_empty() => View::Empty,
            Listing::Loaded => View::List,
        }
    }

    /// Whether the list should be read now without being asked: never read, something
    /// changed, or it is old and nothing is being edited or done.
    pub fn wants_load(&self, now: Instant) -> bool {
        if self.site.is_none() || self.loading.is_some() {
            return false;
        }
        match self.listing {
            Listing::NotLoaded => true,
            // A failure waits for Retry.
            Listing::Failed(_) => false,
            Listing::Loaded => {
                let stale = self.loaded_at.is_none_or(|at| now.saturating_duration_since(at) >= STALE_AFTER);
                self.refresh_wanted || (stale && self.edit == Edit::None && self.working.is_empty())
            }
        }
    }

    /// Start reading the list: the request's number and the site to ask. None when there is
    /// no site or a request is already running.
    pub fn begin_load(&mut self) -> Option<(u64, Site)> {
        let site = self.site.clone()?;
        if self.loading.is_some() {
            return None;
        }
        self.generation += 1;
        self.loading = Some(self.generation);
        self.refresh_wanted = false;
        Some((self.generation, site))
    }

    /// The answer of request `generation`. An answer that is not the running request's (the
    /// site changed, or a job finished since and made it out of date) is dropped.
    pub fn finish_load(&mut self, generation: u64, result: Result<Vec<RemoteTour>, String>, now: Instant) {
        if self.loading != Some(generation) {
            return;
        }
        self.loading = None;
        match result {
            Ok(tours) => {
                self.apply_list(tours);
                self.listing = Listing::Loaded;
                self.loaded_at = Some(now);
            }
            Err(e) => {
                // The rows are not shown with a failure, so nothing on them stays open.
                self.edit = Edit::None;
                self.listing = Listing::Failed(e);
            }
        }
    }

    /// Replace the list. An open delete confirmation survives only when the list is the one
    /// it was asked on: the same tours in the same order, and its tour with the same title
    /// and step count the question names. Otherwise it is closed, never moved.
    fn apply_list(&mut self, tours: Vec<RemoteTour>) {
        match &self.edit {
            Edit::ConfirmDelete { id } => {
                let same_rows = self.tours.len() == tours.len() && self.tours.iter().zip(&tours).all(|(a, b)| a.id == b.id);
                let same_tour = match (self.tour(id), tours.iter().find(|t| &t.id == id)) {
                    (Some(a), Some(b)) => a.title == b.title && a.steps == b.steps,
                    _ => false,
                };
                if !(same_rows && same_tour) {
                    self.edit = Edit::None;
                    self.notice = Some(Notice { text: LIST_CHANGED.to_string(), error: false });
                }
            }
            Edit::Renaming { id, .. } | Edit::ChoosingChapter { id } | Edit::ConfirmPublish { id } => {
                if !tours.iter().any(|t| &t.id == id) {
                    self.edit = Edit::None;
                    self.notice = Some(Notice { text: publish::TOUR_GONE.to_string(), error: false });
                }
            }
            Edit::None => {}
        }
        self.errors.retain(|id, _| tours.iter().any(|t| &t.id == id));
        if self.copied.as_ref().is_some_and(|(id, _)| !tours.iter().any(|t| &t.id == id)) {
            self.copied = None;
        }
        self.tours = tours;
    }

    /// The Refresh button and Retry: read the list again, dropping old messages.
    pub fn refresh(&mut self) -> Option<(u64, Site)> {
        let started = self.begin_load()?;
        self.errors.clear();
        self.notice = None;
        Some(started)
    }

    /// "Rename": the title becomes a text field. Replaces whatever else was open.
    pub fn start_rename(&mut self, id: &str) {
        if self.can_act(id) {
            let text = self.tour(id).map(|t| t.title.clone()).unwrap_or_default();
            self.errors.remove(id);
            self.edit = Edit::Renaming { id: id.to_string(), text };
        }
    }

    /// "Delete": show the confirmation for this tour. Nothing is sent.
    pub fn ask_delete(&mut self, id: &str) {
        if self.can_act(id) {
            self.errors.remove(id);
            self.edit = Edit::ConfirmDelete { id: id.to_string() };
        }
    }

    /// "Edit": the saved recording to open, when it is clear which. No recording that can be
    /// opened puts the reason on the row. A tour with more than one chapter asks which chapter,
    /// also when only one of them is saved, so it is clear that one chapter is edited and not
    /// the whole tour. A recording kept as a video is not asked for when it cannot be opened now
    /// ([`Model::may_open`]). Nothing is read from the disk here.
    pub fn ask_edit(&mut self, id: &str) -> Option<ProjectKey> {
        if !self.can_act(id) {
            return None;
        }
        self.errors.remove(id);
        if self.edit.tour_id() == Some(id) {
            self.edit = Edit::None;
        }
        let chapters = self.tour(id).map_or(0, |t| t.chapters);
        let saved = self.saved(id);
        let can_open = usable(saved).count();
        let Some(key) = usable(saved).next().map(|p| p.key.clone()) else {
            let why = saved.first().and_then(|p| p.problem).map_or(NOT_SAVED, problem_sentence);
            self.errors.insert(id.to_string(), why.to_string());
            return None;
        };
        if chapters > 1 || can_open > 1 {
            self.edit = Edit::ChoosingChapter { id: id.to_string() };
            return None;
        }
        self.may_open(id, key)
    }

    /// The saved recording `key` of the tour `id`, unless it is kept as a video and cannot be
    /// opened now: no ffmpeg was found on this computer, or another recording is being opened.
    /// The row then says which. While ffmpeg is still being looked for, the recording is asked
    /// for: opening it decides.
    fn may_open(&mut self, id: &str, key: ProjectKey) -> Option<ProjectKey> {
        if !self.saved(id).iter().any(|p| p.key == key && p.form == Form::Video) {
            return Some(key);
        }
        let why = if self.no_ffmpeg {
            NEEDS_FFMPEG
        } else if self.opening.is_some() {
            OPENING_ANOTHER
        } else {
            return Some(key);
        };
        self.errors.insert(id.to_string(), why.to_string());
        None
    }

    /// A chapter's button in the chapter question: the saved recording to open. Only a chapter
    /// of the tour the question is about, with a recording that can be opened, is answered.
    pub fn choose_chapter(&mut self, chapter_id: &str) -> Option<ProjectKey> {
        let Edit::ChoosingChapter { id } = &self.edit else { return None };
        let id = id.clone();
        if !self.can_act(&id) {
            return None;
        }
        let key = usable(self.saved(&id)).find(|p| p.key.chapter_id == chapter_id).map(|p| p.key.clone())?;
        self.edit = Edit::None;
        self.may_open(&id, key)
    }

    /// The saved recording of this tour could not be opened after all: the reason goes on its
    /// row, or above the list when the row is gone. Not shown on a page of another site.
    pub fn edit_failed(&mut self, key: &ProjectKey, sentence: &str) {
        if self.site.as_ref().is_none_or(|s| s.server != key.server) {
            return;
        }
        if self.tour(&key.tour_id).is_some() {
            self.errors.insert(key.tour_id.clone(), sentence.to_string());
        } else {
            self.notice = Some(Notice { text: sentence.to_string(), error: true });
        }
    }

    /// How the search for ffmpeg stands, told before every frame. Reads nothing itself.
    pub fn set_ffmpeg(&mut self, state: FfmpegState) {
        self.no_ffmpeg = state == FfmpegState::NotFound;
    }

    /// The folder of the project that was being stored as video when the saved recordings were read.
    pub fn converting(&self) -> Option<&Path> {
        self.converting.as_deref()
    }

    /// The app starts expanding the recording `key` names for an editor; `cancel` stops it.
    /// The tour's row shows the work until [`Model::open_ended`]. False when another recording
    /// is being opened: only one is at a time, and the row of the one refused says so.
    pub fn begin_open(&mut self, key: &ProjectKey, cancel: Arc<AtomicBool>) -> bool {
        let id = key.tour_id.as_str();
        if self.opening.is_some() {
            // A row that shows work has nothing to press, and nothing to be told.
            if self.work(id).is_none() {
                self.edit_failed(key, OPENING_ANOTHER);
            }
            return false;
        }
        self.opening = Some(Opening { key: key.clone(), cancel, progress: None });
        if self.opening_on(id).is_some() {
            if self.edit.tour_id() == Some(id) {
                self.edit = Edit::None;
            }
            self.errors.remove(id);
        }
        true
    }

    /// How far the expansion of `key`'s recording is: frames done, and their number.
    pub fn open_progress(&mut self, key: &ProjectKey, done: usize, total: usize) {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        if let Some(opening) = self.opening.as_mut().filter(|opening| opening.key == *key) {
            opening.progress = Some((count(done), count(total)));
        }
    }

    /// The expansion of `key`'s recording has ended, however it did: the row is an ordinary
    /// one again, another recording may be opened, and a row that was told to wait for this
    /// one no longer says so.
    pub fn open_ended(&mut self, key: &ProjectKey) {
        if self.opening.as_ref().is_none_or(|opening| opening.key != *key) {
            return;
        }
        self.opening = None;
        self.errors.retain(|_, said| said != OPENING_ANOTHER);
    }

    /// "Cancel" on a row whose recording is being opened: the work is told to stop, and the row
    /// is an ordinary one at once. The work lets go within a moment; until it has
    /// ([`Model::open_ended`]) no other recording is opened.
    pub fn cancel_open(&mut self, id: &str) {
        if let Some(opening) = self.opening_on(id) {
            opening.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// Whether a recording is being expanded for an editor.
    #[cfg(test)]
    pub fn is_opening(&self) -> bool {
        self.opening.is_some()
    }

    /// "Cancel" and "Keep it".
    pub fn cancel_edit(&mut self) {
        self.edit = Edit::None;
    }

    #[cfg(test)]
    /// The text field of the rename in progress.
    pub fn rename_text(&mut self) -> Option<&mut String> {
        match &mut self.edit {
            Edit::Renaming { text, .. } => Some(text),
            _ => None,
        }
    }

    /// "Save" of a rename: the job to run, when there is a new name to save. A name that did
    /// not change just closes the field; an empty one keeps it open.
    pub fn save_rename(&mut self) -> Option<Started> {
        let Edit::Renaming { id, text } = &self.edit else { return None };
        let (id, title) = (id.clone(), text.trim().to_string());
        if title.is_empty() || title.chars().count() > TITLE_MAX || !self.can_act(&id) {
            return None;
        }
        self.edit = Edit::None;
        if self.tour(&id).is_some_and(|t| t.title == title) {
            return None;
        }
        self.begin(&id, Work::Renaming, Job::Rename(title))
    }

    /// "Delete tour" in the confirmation. Only the tour whose confirmation is showing can be
    /// deleted: `id` is the row the button was drawn on, and it must be that tour. The
    /// confirmation closes with the click, so a second click finds nothing to confirm.
    pub fn confirm_delete(&mut self, id: &str) -> Option<Started> {
        if self.edit != (Edit::ConfirmDelete { id: id.to_string() }) || !self.can_act(id) {
            return None;
        }
        self.edit = Edit::None;
        self.begin(id, Work::Deleting(None), Job::Delete)
    }

    /// "Publish" on a draft.
    pub fn start_publish(&mut self, id: &str) -> Option<Started> {
        if !self.can_act(id) || self.tour(id).is_none_or(RemoteTour::is_public) {
            return None;
        }
        self.begin(id, Work::Publishing(None), Job::Publish)
    }

    /// "Publish" on a draft as the row's button asks for it: a tour whose restricted publish
    /// did not finish first asks whether to publish it for anyone ([`Edit::ConfirmPublish`]);
    /// any other starts at once, as [`Model::start_publish`].
    pub fn request_publish(&mut self, id: &str) -> Option<Started> {
        if !publish_ui::restriction_unfinished(id) {
            return self.start_publish(id);
        }
        if self.can_act(id) && self.tour(id).is_some_and(|t| !t.is_public()) {
            self.errors.remove(id);
            self.edit = Edit::ConfirmPublish { id: id.to_string() };
        }
        None
    }

    /// "Publish for anyone" in that question: only for the tour whose question is showing. The
    /// question closes with the click, and the tour is no longer asked about.
    pub fn confirm_publish(&mut self, id: &str) -> Option<Started> {
        if self.edit != (Edit::ConfirmPublish { id: id.to_string() }) || !self.can_act(id) {
            return None;
        }
        self.edit = Edit::None;
        publish_ui::settle_restriction(id);
        self.start_publish(id)
    }

    /// "Publish changes" on a public tour whose live page is behind its draft: the job a draft's
    /// Publish runs, for the same link. It asks nothing, and starts only where the button shows
    /// and while nothing else runs on the tour.
    pub fn start_publish_changes(&mut self, id: &str) -> Option<Started> {
        if !self.can_act(id) || !self.tour(id).is_some_and(RemoteTour::changes_waiting) {
            return None;
        }
        self.begin(id, Work::Publishing(None), Job::Publish)
    }

    /// "Unpublish" on a public tour.
    pub fn start_unpublish(&mut self, id: &str) -> Option<Started> {
        if !self.can_act(id) || !self.tour(id).is_some_and(RemoteTour::is_public) {
            return None;
        }
        self.begin(id, Work::Unpublishing, Job::Unpublish)
    }

    /// "Finish deleting" on an unfinished delete's block: send the delete for that tour again.
    /// The entry must be one of this site's, and its tour must not be in the list any more
    /// (a tour that is there is deleted through its own confirmation, never from here).
    pub fn finish_delete(&mut self, entry: &UnfinishedDelete) -> Option<Started> {
        let site = self.site.clone()?;
        let id = entry.tour_id.as_str();
        if entry.server != site.server || !publish::is_tour_id(id) || self.tour(id).is_some() || self.working.contains_key(id) {
            return None;
        }
        self.working.insert(id.to_string(), Work::Deleting(None));
        Some(Started { site, id: id.to_string(), title: entry.title.clone(), job: Job::Delete, listed: false })
    }

    fn begin(&mut self, id: &str, work: Work, job: Job) -> Option<Started> {
        let site = self.site.clone()?;
        // An edit of this tour ends when a job on it starts.
        if self.edit.tour_id() == Some(id) {
            self.edit = Edit::None;
        }
        self.errors.remove(id);
        self.working.insert(id.to_string(), work);
        let (title, listed) = self.tour(id).map_or((String::new(), false), |t| (t.title.clone(), true));
        Some(Started { site, id: id.to_string(), title, job, listed })
    }

    /// A running job's progress.
    pub fn progress(&mut self, id: &str, work: Work) {
        if let Some(w) = self.working.get_mut(id) {
            *w = work;
        }
    }

    /// Whether an unfinished delete has its block: once its tour is out of the list. While
    /// the row is still there, the row itself says what is going on.
    pub fn shows_unfinished(&self, entry: &UnfinishedDelete) -> bool {
        self.site.as_ref().is_some_and(|s| s.server == entry.server) && self.tour(&entry.tour_id).is_none()
    }

    /// A job ended. The list is changed to what the job did and read again, so the page
    /// shows what the site says. A result for another site than the page's is dropped.
    pub fn finish(&mut self, started: &Started, result: Result<Done, Failure>) {
        let id = started.id.as_str();
        self.working.remove(id);
        if self.site.as_ref() != Some(&started.site) {
            return;
        }
        // A list request that is running started before this result: its answer is out of date.
        self.loading = None;
        self.generation += 1;
        self.refresh_wanted = true;
        let removed = |m: &mut Model| {
            m.tours.retain(|t| t.id != id);
            m.errors.remove(id);
            if m.edit.tour_id() == Some(id) {
                m.edit = Edit::None;
            }
        };
        match result {
            Ok(Done::Renamed(title)) => {
                if let Some(t) = self.tour_mut(id) {
                    t.title = title;
                }
            }
            Ok(Done::Published(link)) => {
                if let Some(t) = self.tour_mut(id) {
                    t.status = "published".to_string();
                    t.link = link;
                    t.published_at = None;
                    // Everything that waited is live now.
                    t.unpublished_changes = false;
                }
            }
            Ok(Done::Unpublished) => {
                if let Some(t) = self.tour_mut(id) {
                    t.status = "draft".to_string();
                    t.link = None;
                    t.published_at = None;
                    t.unpublished_changes = false;
                }
            }
            Ok(Done::Deleted) => removed(self),
            Ok(Done::DeletedSavedLeft) => {
                removed(self);
                self.notice = Some(Notice { text: SAVED_NOT_REMOVED.to_string(), error: true });
            }
            Err(Failure::Gone) => {
                removed(self);
                self.notice = Some(Notice { text: publish::TOUR_GONE.to_string(), error: false });
            }
            Err(Failure::DeleteUnfinished(text)) => {
                // The way back to it is its own block, which this message cannot replace or lose.
                removed(self);
                self.notice = Some(Notice { text, error: true });
            }
            Err(Failure::Message(text)) => {
                if self.tour(id).is_some() {
                    self.errors.insert(id.to_string(), text);
                } else {
                    self.notice = Some(Notice { text, error: true });
                }
            }
        }
    }

    /// "Copy link" was pressed on this tour.
    pub fn copied(&mut self, id: &str, now: Instant) {
        self.copied = Some((id.to_string(), now));
    }

    /// Whether "Link copied" shows beside this tour.
    pub fn shows_copied(&self, id: &str, now: Instant) -> bool {
        self.copied.as_ref().is_some_and(|(c, at)| c == id && now.saturating_duration_since(*at) < COPIED_FOR)
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }
}

/// The buttons of the question before publishing for anyone, left to right, and whether each is
/// the default (what Enter and Escape choose).
pub fn confirm_publish_buttons() -> [(&'static str, bool); 2] {
    [(crate::share_ui::who::CONFIRM_CANCEL, true), (crate::share_ui::who::CONFIRM_PUBLISH, false)]
}

/// Delete a tour a restricted publish created and could not publish as asked, the way the page
/// deletes one: noted before the request goes (and only when the note could be written), the
/// note removed once the site says nothing is left. True when the tour is gone. The saved
/// recording on this computer is left alone.
pub fn delete_created_tour(server: &str, token: &str, tour_id: &str, title: &str) -> bool {
    let Some(unfinished) = SHARED_UNFINISHED.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return false };
    let Ok(server) = publish::check_server(server) else { return false };
    let noted_here = match unfinished.add(&server, tour_id, title) {
        Noted::Now => true,
        Noted::Before => false,
        // No note, no delete: the page's own rule.
        Noted::NotSaved => return false,
    };
    match publish::delete_tour(&server, token, tour_id, |_| {}) {
        Ok(_) => {
            unfinished.remove(&server, tour_id);
            true
        }
        // The tour itself is gone; its note stays for "Finish deleting".
        Err(f) if f.unfinished => true,
        Err(f) => {
            if f.not_started && noted_here {
                unfinished.remove(&server, tour_id);
            }
            false
        }
    }
}

/// The app's list of deletes that did not finish, once it has started ([`Unfinished::start`]),
/// for a delete sent from an editor.
static SHARED_UNFINISHED: Mutex<Option<Unfinished>> = Mutex::new(None);

// ---- Deletes that did not finish ----

/// The deletes that were sent and are not known to be complete: the site may have removed
/// the tour but not yet all of its files. One list, shared by the page, its jobs and the
/// app. A job notes its delete here, and on disk, before the first request goes out, so no
/// lost answer and no crash can leave files on the site with nothing pointing at them; the
/// note goes when the site says nothing is left, or that it never began. The app copies
/// every change into the settings file, from where it is read back at start-up. Entries of
/// other sites than the configured one are kept, and shown when that site is configured again.
#[derive(Clone, Default)]
pub struct Unfinished(Arc<Mutex<UnfinishedList>>);

/// Writes the list to the settings file at once; false when it could not.
type SaveNow = Arc<dyn Fn(&[UnfinishedDelete]) -> bool + Send + Sync>;

/// What became of the note for a delete that is about to be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noted {
    /// Written to the settings file just now.
    Now,
    /// There already: an earlier delete of this tour did not finish.
    Before,
    /// It could not be written to the settings file, so there is no note, and the delete must not be sent.
    NotSaved,
}

#[derive(Default)]
struct UnfinishedList {
    list: Vec<UnfinishedDelete>,
    /// The app's copy of the settings does not hold this list yet.
    changed: bool,
    save_now: Option<SaveNow>,
}

impl Unfinished {
    fn lock(&self) -> MutexGuard<'_, UnfinishedList> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// At start-up: the list as the settings file holds it.
    pub fn load(&self, list: Vec<UnfinishedDelete>) {
        let mut l = self.lock();
        l.list = list;
        l.changed = false;
    }

    /// How a note gets to the settings file before its request is sent. Set once by the app.
    /// Until it is set no new note can be made, so no new delete is sent.
    pub fn save_with(&self, save_now: impl Fn(&[UnfinishedDelete]) -> bool + Send + Sync + 'static) {
        self.lock().save_now = Some(Arc::new(save_now));
    }

    /// Note that a delete of this tour on `server` (an address as the address rule writes it)
    /// is about to be sent, and write the note to the settings file before returning.
    pub fn add(&self, server: &str, tour_id: &str, title: &str) -> Noted {
        let mut l = self.lock();
        if l.list.iter().any(|e| e.server == server && e.tour_id == tour_id) {
            return Noted::Before;
        }
        let title = if title.trim().is_empty() { "Untitled tour".to_string() } else { title.chars().take(TITLE_MAX).collect() };
        let mut list = l.list.clone();
        list.push(UnfinishedDelete { server: server.to_string(), tour_id: tour_id.to_string(), title });
        let extra = list.len().saturating_sub(MAX_UNFINISHED);
        list.drain(..extra);
        // Written while the list is held, so the app's own save cannot come between with an older list.
        // A note that is not on disk is no note: the list stays as it was, and with no way to
        // write (the settings file is not to be touched this session) there is none either.
        if !l.save_now.clone().is_some_and(|save| save(&list)) {
            return Noted::NotSaved;
        }
        l.list = list;
        // The app's copy still has to learn of it, or its next save would take the note away again.
        l.changed = true;
        Noted::Now
    }

    /// With the list held still: `write` gets the list if the app's copy does not have it
    /// yet, and writes the settings file, answering whether that worked. No note can be added,
    /// or written, half-way through. A write that failed leaves the list to be written again.
    pub fn settle(&self, write: impl FnOnce(Option<Vec<UnfinishedDelete>>) -> bool) -> bool {
        let mut l = self.lock();
        let saved = write(l.changed.then(|| l.list.clone()));
        if saved {
            l.changed = false;
        }
        saved
    }

    /// At start-up: the list as the settings file holds it, and how a new note gets there
    /// (see [`crate::settings::Settings::note_saver`]). With no way to write a note, no new delete is sent.
    pub fn start(&self, list: Vec<UnfinishedDelete>, save_now: Option<impl Fn(&[UnfinishedDelete]) -> bool + Send + Sync + 'static>) {
        self.load(list);
        if let Some(save_now) = save_now {
            self.save_with(save_now);
        }
        // Not from tests: they start lists of their own, which no editor should write to.
        if !cfg!(test) {
            *SHARED_UNFINISHED.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.clone());
        }
    }

    /// The site said nothing of this tour is left, or that it never began to delete it.
    pub fn remove(&self, server: &str, tour_id: &str) {
        let mut l = self.lock();
        let before = l.list.len();
        l.list.retain(|e| !(e.server == server && e.tour_id == tour_id));
        l.changed |= l.list.len() != before;
    }

    /// The entries of the site at `server`, oldest first. An entry that does not name a tour id is never offered.
    pub fn for_site(&self, server: &str) -> Vec<UnfinishedDelete> {
        self.lock().list.iter().filter(|e| e.server == server && publish::is_tour_id(&e.tour_id)).cloned().collect()
    }

    /// The whole list, once, when it changed since the last call: what the settings file should hold now.
    #[cfg(test)]
    pub fn take_changed(&self) -> Option<Vec<UnfinishedDelete>> {
        let mut taken = None;
        self.settle(|list| {
            taken = list;
            true
        });
        taken
    }

    pub fn is_changed(&self) -> bool {
        self.lock().changed
    }
}

// ---- Running it ----

/// Where a job gets the token for the site address it is about to use.
type TokenSource = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// Where the recordings saved on this computer are. None: this user has no folder for them, so
/// none is saved. Asked on a background thread only.
type Projects = Arc<dyn Fn() -> Option<Store> + Send + Sync>;

/// The tours the site said are gone, in this session, by address and tour id. An upload that
/// was still saving its recording when its tour was deleted leaves that recording behind; it
/// is found by this the next time the saved recordings are read. A tour that a list read
/// later holds is taken out again: the site has it.
#[derive(Clone, Default)]
struct Gone(Arc<Mutex<HashSet<(String, String)>>>);

impl Gone {
    fn lock(&self) -> MutexGuard<'_, HashSet<(String, String)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn add(&self, server: &str, tour_id: &str) {
        self.lock().insert((server.to_string(), tour_id.to_string()));
    }

    fn forget(&self, server: &str, tour_id: &str) {
        self.lock().remove(&(server.to_string(), tour_id.to_string()));
    }

    /// The site lists these tours: whatever was said before, they are not gone. A recording
    /// saved for one of them is never removed on the strength of an older answer.
    fn listed<'a>(&self, server: &str, ids: impl Iterator<Item = &'a str>) {
        let mut gone = self.lock();
        for id in ids {
            gone.remove(&(server.to_string(), id.to_string()));
        }
    }

    /// The tours among `ids` the site said are gone.
    fn of<'a>(&self, server: &str, ids: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
        let gone = self.lock();
        ids.filter(|id| gone.contains(&(server.to_string(), id.to_string()))).map(str::to_string).collect()
    }
}

/// The deletes asked for on a tour's own row in this session, by site (address and token) and
/// tour id: tours that the list read with that very token held. The site answers "no such
/// tour" for a tour of another workspace exactly as for one that is gone, so that answer
/// removes a saved recording only for a tour known this way to have been this workspace's.
#[derive(Clone, Default)]
struct Listed(Arc<Mutex<HashSet<(Site, String)>>>);

impl Listed {
    fn lock(&self) -> MutexGuard<'_, HashSet<(Site, String)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn add(&self, site: &Site, tour_id: &str) {
        self.lock().insert((site.clone(), tour_id.to_string()));
    }

    fn has(&self, site: &Site, tour_id: &str) -> bool {
        self.lock().contains(&(site.clone(), tour_id.to_string()))
    }
}

/// How a read of the saved recordings learns which project is being stored as video.
type Converting = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

/// What a job or a read of the saved recordings works with besides the model.
#[derive(Clone)]
struct Tools {
    token: TokenSource,
    unfinished: Unfinished,
    projects: Projects,
    gone: Gone,
    listed: Listed,
    /// The folder of the project that is being stored as video now ([`project::converting_now`]).
    converting: Converting,
}

/// The page's state: the model, shared with the jobs, the unfinished deletes, where a job gets
/// the token, and where the saved recordings are.
pub struct ToursState {
    model: Arc<Mutex<Model>>,
    tools: Tools,
}

impl Default for ToursState {
    fn default() -> Self {
        let tools = Tools { token: Arc::new(publish_ui::token_for), unfinished: Unfinished::default(), projects: Arc::new(|| Store::default_root().map(Store::at)), gone: Gone::default(), listed: Listed::default(), converting: Arc::new(project::converting_now) };
        Self { model: Arc::default(), tools }
    }
}

/// The site said this tour is gone (its delete succeeded, the delete is under way with only
/// files left, or a tour this workspace's list held is no longer there): remove the recordings
/// saved on this computer for it. Never call it for a delete whose outcome is not known, nor
/// for "no such tour" about a tour that is not known to have been this workspace's. One that
/// is open in an editor goes when that editor closes. False when one could not be removed yet;
/// the store finishes that at the next start. `server` is the address as
/// [`publish::check_server`] writes it.
fn forget_saved(tools: &Tools, server: &str, tour_id: &str) -> bool {
    // Nothing is saved under an address the address rule would not write.
    let Ok(server) = publish::check_server(server) else { return true };
    tools.gone.add(&server, tour_id);
    match (tools.projects)() {
        Some(store) => store.remove_tour(&server, tour_id).is_ok(),
        None => true,
    }
}

/// The recordings saved on this computer for the site at `server` (an address as
/// [`publish::check_server`] writes it), in one walk of its folder, and the store's counter
/// from before the walk. No frames are read. A recording of a tour the site said is gone,
/// saved by an upload that was still running when the tour was deleted, is removed here.
/// Never call it on the window's thread.
fn read_saved(tools: &Tools, server: &str) -> (u64, Vec<ProjectInfo>, Option<PathBuf>) {
    let epoch = project::epoch();
    let Some(store) = (tools.projects)() else { return (epoch, Vec::new(), None) };
    // After the counter: a conversion that starts or ends from here on moves it, and is read again.
    let converting = (tools.converting)();
    let list = store.for_site(server);
    let late = tools.gone.of(server, list.iter().map(|p| p.key.tour_id.as_str()));
    if late.is_empty() {
        return (epoch, list, converting);
    }
    for id in &late {
        if store.remove_tour(server, id).is_err() {
            // Not tried again in this session; the store finishes it at the next start.
            tools.gone.forget(server, id);
        }
    }
    (epoch, store.for_site(server), converting)
}

/// Read the site's tour list, and with it what is saved on this computer for the site. The
/// list is asked for first: a tour it holds is on the site, so it is taken out of the tours
/// said to be gone before the saved recordings are looked at, and a recording of it is kept.
/// Never call it on the window's thread.
fn read_list(tools: &Tools, site: &Site) -> (u64, Vec<ProjectInfo>, Option<PathBuf>, Result<Vec<RemoteTour>, String>) {
    let result = (tools.token)(&site.server).and_then(|token| publish::list_tours(&site.server, &token));
    if let Ok(tours) = &result {
        tools.gone.listed(&site.server, tours.iter().map(|t| t.id.as_str()));
    }
    let (epoch, saved, converting) = read_saved(tools, &site.server);
    (epoch, saved, converting, result)
}

fn lock(model: &Mutex<Model>) -> MutexGuard<'_, Model> {
    // A job that panicked must not take the page down with it.
    model.lock().unwrap_or_else(|e| e.into_inner())
}

fn failure(e: String) -> Failure {
    if publish::is_gone(&e) { Failure::Gone } else { Failure::Message(e) }
}

/// Ends a list request that never reported back (its thread panicked), so the page does not wait for ever.
struct LoadGuard {
    model: Arc<Mutex<Model>>,
    generation: u64,
    armed: bool,
}

impl Drop for LoadGuard {
    fn drop(&mut self) {
        if self.armed {
            lock(&self.model).finish_load(self.generation, Err(LOAD_BROKE.to_string()), Instant::now());
        }
    }
}

/// Ends a read of the saved recordings that never reported back, so the next change is read again.
struct SavedGuard {
    model: Arc<Mutex<Model>>,
    armed: bool,
}

impl Drop for SavedGuard {
    fn drop(&mut self) {
        if self.armed {
            lock(&self.model).saved_reading = false;
        }
    }
}

/// The same for a job: its tour is released and gets a sentence.
struct JobGuard {
    model: Arc<Mutex<Model>>,
    started: Started,
    armed: bool,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if self.armed {
            lock(&self.model).finish(&self.started, Err(Failure::Message(JOB_BROKE.to_string())));
        }
    }
}

/// Do the job's requests. A delete is noted, in the settings file too, before its first
/// request is sent: the answer to that request can be lost after the site has removed the
/// tour. A delete whose note cannot be written is not sent at all. The note goes when the site says nothing is left (a tour that is already gone
/// included), or when it clearly refused before beginning (a publish in progress, a
/// rejected token, a site too old to delete), unless the note is an earlier delete's. Any
/// other end (no answer, a server error, an answer that makes no sense) leaves the note:
/// files may be there.
///
/// The recordings saved on this computer for the tour are removed only once the site has said
/// the tour is gone: after a delete the site answered for this tour (it removed it, or left
/// only files behind), and after "no such tour" for a tour this workspace is known to have had
/// ([`knows_tour`]). The site says "no such tour" of another workspace's tour too, so for
/// "Finish deleting" with a note from an earlier session that answer alone removes nothing: the
/// saved recording stays for the user to remove. A refusal, a lost answer or a server error
/// removes nothing. A recording that could not be removed never turns the delete into a failure.
fn run_job(model: &Mutex<Model>, tools: &Tools, started: &Started) -> Result<Done, Failure> {
    let (server, id) = (started.site.server.as_str(), started.id.as_str());
    let unfinished = &tools.unfinished;
    let token = (tools.token)(server).map_err(Failure::Message)?;
    match &started.job {
        Job::Rename(title) => publish::rename_tour(server, &token, id, title).map(Done::Renamed).map_err(failure),
        Job::Unpublish => publish::unpublish_tour(server, &token, id).map(|()| Done::Unpublished).map_err(failure),
        Job::Publish => publish::publish_tour(server, &token, id, |images| {
            lock(model).progress(id, Work::Publishing(images));
            true
        })
        .map(|live| Done::Published(live.url))
        .map_err(failure),
        Job::Delete => {
            let noted_here = match unfinished.add(server, id, &started.title) {
                Noted::Now => true,
                Noted::Before => false,
                Noted::NotSaved => return Err(Failure::Message(NOTE_NOT_SAVED.to_string())),
            };
            let known = knows_tour(tools, started);
            let result = publish::delete_tour(server, &token, id, |files| lock(model).progress(id, Work::Deleting(files)));
            match result {
                Ok(how) => {
                    unfinished.remove(server, id);
                    // "No such tour" for a tour not known to have been this workspace's says
                    // nothing about the tour the recording was saved for: it is left alone.
                    let removed = if how == publish::Deleted::BySite || known { forget_saved(tools, server, id) } else { true };
                    Ok(if removed { Done::Deleted } else { Done::DeletedSavedLeft })
                }
                // The site began: the tour itself is gone, whatever is left of its files.
                Err(f) if f.unfinished => Err(Failure::DeleteUnfinished(if forget_saved(tools, server, id) { f.message } else { format!("{} {SAVED_NOT_REMOVED}", f.message) })),
                Err(f) => {
                    if f.not_started && noted_here {
                        unfinished.remove(server, id);
                    }
                    Err(Failure::Message(f.message))
                }
            }
        }
    }
}

/// Whether the tour of a delete is known to have been this workspace's: the list read with this
/// token held it when the delete was asked for, now or earlier in this session (a delete whose
/// answer was lost is carried on by "Finish deleting", for a tour the list no longer holds).
/// A delete asked for on a row is remembered for that.
fn knows_tour(tools: &Tools, started: &Started) -> bool {
    if started.listed {
        tools.listed.add(&started.site, &started.id);
    }
    started.listed || tools.listed.has(&started.site, &started.id)
}

impl ToursState {
    /// A page whose jobs get the token from `token` instead of the credential store.
    /// With no folder for saved recordings, so a test never looks at this computer's own.
    #[cfg(test)]
    fn with_token(token: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Self {
        let tools = Tools { token: Arc::new(token), unfinished: Unfinished::default(), projects: Arc::new(|| None), gone: Gone::default(), listed: Listed::default(), converting: Arc::new(|| None) };
        Self { model: Arc::default(), tools }
    }

    /// The same page with its saved recordings in `store`.
    #[cfg(test)]
    fn with_store(mut self, store: Store) -> Self {
        self.tools.projects = Arc::new(move || Some(store.clone()));
        self
    }

    /// The unfinished deletes, for the app to load at start-up and to write to the settings file.
    pub fn unfinished(&self) -> Unfinished {
        self.tools.unfinished.clone()
    }

    /// The app could not open the saved recording `key` names: say why on that tour's row.
    pub fn edit_failed(&self, key: &ProjectKey, sentence: &str) {
        lock(&self.model).edit_failed(key, sentence);
    }

    /// [`Model::begin_open`], for the app.
    pub fn begin_open(&self, key: &ProjectKey, cancel: Arc<AtomicBool>) -> bool {
        lock(&self.model).begin_open(key, cancel)
    }

    /// [`Model::open_ended`], for the app.
    pub fn open_ended(&self, key: &ProjectKey) {
        lock(&self.model).open_ended(key);
    }

    /// Where the thread that expands a recording reports how far it is.
    pub fn open_rows(&self) -> OpenRows {
        OpenRows(self.model.clone())
    }

    /// Read the list, and with it what is saved on this computer for the site.
    fn spawn_load(&self, (generation, site): (u64, Site)) {
        let (model, tools) = (self.model.clone(), self.tools.clone());
        std::thread::spawn(move || {
            let mut guard = LoadGuard { model, generation, armed: true };
            let (epoch, saved, converting, result) = read_list(&tools, &site);
            guard.armed = false;
            let mut m = lock(&guard.model);
            m.apply_saved(&site, epoch, saved, converting);
            m.finish_load(generation, result, Instant::now());
        });
    }

    /// Read what is saved on this computer again: something was saved or removed.
    fn spawn_saved(&self, site: Site) {
        let (model, tools) = (self.model.clone(), self.tools.clone());
        std::thread::spawn(move || {
            let mut guard = SavedGuard { model, armed: true };
            let (epoch, saved, converting) = read_saved(&tools, &site.server);
            guard.armed = false;
            lock(&guard.model).finish_saved(&site, epoch, saved, converting);
        });
    }

    fn spawn_job(&self, started: Started) {
        let (model, tools) = (self.model.clone(), self.tools.clone());
        std::thread::spawn(move || {
            let mut guard = JobGuard { model, started, armed: true };
            let result = run_job(&guard.model, &tools, &guard.started);
            guard.armed = false;
            lock(&guard.model).finish(&guard.started, result);
        });
    }
}

// ---- Opening a recording that is kept as a video ----

/// The rows of the page, for the thread that expands a recording: it reports into the model, as
/// the page's own jobs do, and never waits for the window.
#[derive(Clone, Default)]
pub struct OpenRows(Arc<Mutex<Model>>);

impl OpenRows {
    /// [`Model::open_progress`].
    pub fn progress(&self, key: &ProjectKey, done: usize, total: usize) {
        lock(&self.0).open_progress(key, done, total);
    }
}

/// What expanding a recording kept as a video works with.
pub struct VideoOpen<'a> {
    /// The folder of saved recordings; None when this user has none.
    pub store: Option<&'a Store>,
    /// What reads the video; None when there is no ffmpeg that can.
    pub codec: Option<&'a dyn VideoCodec>,
    /// The folder the frames are expanded into, see [`new_edit_folder`]: not there yet.
    pub temp: &'a Path,
    /// Set to stop the work: by Cancel on the row, or when the app closes.
    pub cancel: &'a AtomicBool,
    /// The room left on the disk a folder is on, see [`project::free_bytes`].
    pub free: &'a (dyn Fn(&Path) -> Option<u64> + Sync),
}

/// A folder is older than this before [`sweep_stale_edits`] takes it.
pub const EDIT_STALE_AFTER: Duration = Duration::from_secs(60 * 60);
/// The frames file of an expanded recording, the only thing its folder holds.
const EDIT_FRAMES: &str = "frames.zst";

/// Where recordings are expanded for an editor: `Mythicshot` in the temporary folder.
pub fn edit_base() -> PathBuf {
    std::env::temp_dir().join("Mythicshot")
}

/// A new name for the folder one recording is expanded into: `edit-<pid>-<nanos>` in `base`,
/// as [`sweep_stale_edits`] knows it. Two calls in one process never answer the same name.
pub fn new_edit_folder(base: &Path) -> PathBuf {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX - 1));
    // The clock can answer the same instant twice.
    let next = |last: u64| now.max(last.saturating_add(1));
    let nanos = next(LAST.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |last| Some(next(last))).unwrap_or(0));
    base.join(format!("edit-{}-{nanos}", std::process::id()))
}

/// The body of the thread that opens a recording kept as a video: expand it into `open.temp`,
/// reporting every frame to the row, and hand what came of it to `done`, once. This can take
/// minutes; `open.cancel` stops it at the next frame.
///
/// Whatever does not end with a recording changes nothing in the project and leaves no folder
/// at `open.temp`. A recording that is handed over owns that folder: whoever takes it removes
/// it as it removes any recording's ([`mythicshot_recording::Recording::delete_files`]).
pub fn open_video(open: &VideoOpen<'_>, key: &ProjectKey, rows: &OpenRows, done: impl FnOnce(Result<Loaded, LoadError>)) {
    // Work that breaks off still reports, or the row would say "opening" for the rest of the run.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| expand(open, key, rows))).unwrap_or_else(|_| Err(LoadError::Damaged("opening the recording broke off".into())));
    if result.is_err() {
        remove_edit_folder(open.temp);
    }
    done(result);
}

fn expand(open: &VideoOpen<'_>, key: &ProjectKey, rows: &OpenRows) -> Result<Loaded, LoadError> {
    let store = open.store.ok_or(LoadError::NotSaved)?;
    let stopped = || open.cancel.load(Ordering::SeqCst);
    if stopped() {
        return Err(LoadError::Cancelled);
    }
    // Without anything to read the video with, the store says so; room is asked about only
    // when the work would start.
    if open.codec.is_some() && !has_room(open, store, key) {
        // Too little room is the answer only when nothing else is in the way: the project is
        // asked for without anything to read its video with, which expands nothing and makes
        // no folder, and whatever it says besides "needs ffmpeg" is the truer answer.
        return match store.load_with(key, &Expand { codec: None, temp: open.temp, keep_going: &|_, _| false }) {
            Err(LoadError::NeedsFfmpeg) => Err(LoadError::NoRoom),
            other => other,
        };
    }
    let keep_going = |done: usize, total: usize| {
        rows.progress(key, done, total);
        !stopped()
    };
    store.load_with(key, &Expand { codec: open.codec, temp: open.temp, keep_going: &keep_going })
}

/// Whether the disk the frames are expanded on has room for them, so that a full disk is said
/// at once and not after minutes of work. What they need is what they took before they were
/// stored as video: for a project kept as a video that is the size of the frames its project
/// file names (the swap, and a save that is born as a video, write it and the `lossless_bytes`
/// of the file's `store` section as one number). A size that is not known (0), a project that
/// is not listed as a video, and a disk that does not say all count as room: the expansion
/// itself reports a disk that fills.
fn has_room(open: &VideoOpen<'_>, store: &Store, key: &ProjectKey) -> bool {
    let needs = store.for_site(&key.server).into_iter().find(|p| p.key == *key && p.form == Form::Video).map_or(0, |p| p.frames_bytes);
    if needs == 0 {
        return true;
    }
    // The folder is not made yet: the nearest one above it that is there answers for its disk.
    open.temp.ancestors().find_map(|folder| (open.free)(folder)).is_none_or(|free| free >= needs)
}

/// A folder, and not a link or a junction to one.
fn is_real_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir() && !is_reparse_point(&m))
}

/// A file, and not a link to one.
fn is_real_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file() && !is_reparse_point(&m))
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

/// Whether the folder `dir` holds what an expansion leaves and nothing else: `Some(true)` for
/// exactly its frames file, `Some(false)` for nothing at all, None for anything else or a
/// folder that cannot be read.
fn only_frames(dir: &Path) -> Option<bool> {
    let mut holds = false;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        if holds || entry.file_name() != EDIT_FRAMES || !is_real_file(&entry.path()) {
            return None;
        }
        holds = true;
    }
    Some(holds)
}

/// Remove the folder a recording was expanded into, when it is a real folder (not a link or a
/// junction) that holds nothing, or nothing but its frames file. Anything else is left whole.
pub fn remove_edit_folder(dir: &Path) {
    if !is_real_folder(dir) {
        return;
    }
    match only_frames(dir) {
        Some(true) if std::fs::remove_file(dir.join(EDIT_FRAMES)).is_err() => return,
        Some(_) => {}
        None => return,
    }
    let _ = std::fs::remove_dir(dir);
}

/// The process number in a folder name of exactly the form `edit-<digits>-<digits>`.
fn edit_pid(name: &str) -> Option<u32> {
    let (pid, nanos) = name.strip_prefix("edit-")?.split_once('-')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits(pid) || !digits(nanos) {
        return None;
    }
    // A number no process has: nobody's, and so not this run's either.
    Some(pid.parse().unwrap_or(u32::MAX))
}

/// At start-up: remove the folders in `base` that expansions of an earlier run left behind when
/// it was cut off. Only a real folder (not a link or a junction) named exactly
/// `edit-<digits>-<digits>` whose first number is not `own_pid`, that holds nothing or nothing
/// but its frames file, and that was last changed, like that file, at least `age` ago.
/// Never call it on the window's thread.
pub fn sweep_stale_edits(base: &Path, own_pid: u32, age: Duration) {
    if !is_real_folder(base) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(base) else { return };
    let now = std::time::SystemTime::now();
    let old = |path: &Path| std::fs::symlink_metadata(path).and_then(|m| m.modified()).is_ok_and(|at| now.duration_since(at).is_ok_and(|since| since >= age));
    for entry in entries.flatten() {
        let dir = entry.path();
        let theirs = entry.file_name().to_str().and_then(edit_pid).is_some_and(|pid| pid != own_pid);
        if !theirs || !is_real_folder(&dir) || !old(&dir) {
            continue;
        }
        match only_frames(&dir) {
            Some(true) if !old(&dir.join(EDIT_FRAMES)) => {}
            Some(_) => remove_edit_folder(&dir),
            None => {}
        }
    }
}

/// What was pressed in a frame; applied after drawing.
enum Click {
    Refresh,
    RecordTour,
    Workspace,
    Button(String, RowButton, Option<String>),
    SaveRename,
    CancelEdit,
    /// A chapter's button in the chapter question, by chapter id.
    ChooseChapter(String),
    /// Cancel on a row whose recording is being opened, by tour id.
    CancelOpen(String),
    DeleteTour(String),
    /// "Publish for anyone" in the question for a tour whose restriction was not applied.
    PublishForAnyone(String),
    FinishDelete(UnfinishedDelete),
    DismissNotice,
}

/// Draw the page.
pub fn ui(ui: &mut egui::Ui, state: &mut ToursState, conn: &Conn, out: &mut PageOut) {
    let now = Instant::now();
    let mut m = lock(&state.model);
    m.set_site(conn.site());
    m.set_ffmpeg(project::ffmpeg_state());
    if m.wants_load(now)
        && let Some(load) = m.begin_load()
    {
        state.spawn_load(load);
    }
    // The disk is never read here: a change the store counted is read on a thread of its own.
    if m.wants_saved(project::epoch())
        && let Some(site) = m.begin_saved()
    {
        state.spawn_saved(site);
    }
    let view = m.view(conn);
    let mut clicks = Vec::new();

    ui.horizontal(|ui| {
        ui.heading("Tours");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(theme::primary_button(RECORD_A_TOUR)).on_hover_text("Go to Home, where a recording starts").clicked() {
                clicks.push(Click::RecordTour);
            }
            let can_refresh = m.site.is_some() && !m.is_loading();
            if refresh_button(ui, can_refresh).clicked() {
                clicks.push(Click::Refresh);
            }
            if m.is_loading() && m.loaded_at.is_some() {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
    });
    ui.add_space(10.0);

    if let Some(notice) = m.notice().cloned() {
        notice_ui(ui, &notice, &mut clicks);
    }
    // Deletes that did not finish on this site: always shown, whatever else the page says.
    if let Some(site) = m.site.clone() {
        for entry in state.tools.unfinished.for_site(&site.server).into_iter().filter(|e| m.shows_unfinished(e)) {
            unfinished_ui(ui, &entry, m.work(&entry.tour_id), &mut clicks);
        }
    }

    match view {
        View::NotConnected(why) => {
            ui.add(egui::Label::new(RichText::new(why).color(theme::MUTED)).wrap());
            ui.add_space(8.0);
            if ui.button(OPEN_WORKSPACE).clicked() {
                clicks.push(Click::Workspace);
            }
        }
        View::Checking => {
            ui.label(RichText::new(CHECKING).color(theme::MUTED));
        }
        View::Loading => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new(LOADING).color(theme::MUTED));
            });
        }
        View::Failed(e) => {
            ui.add(egui::Label::new(RichText::new(e).color(theme::DANGER_TEXT)).wrap());
            ui.add_space(8.0);
            if ui.add_enabled(!m.is_loading(), egui::Button::new("Retry")).clicked() {
                clicks.push(Click::Refresh);
            }
        }
        View::Empty => {
            ui.add(egui::Label::new(RichText::new(EMPTY).color(theme::MUTED)).wrap());
        }
        View::List => {
            let today = Local::now();
            // The edit is taken out while the rows are drawn, so the rename field can be typed into.
            let mut edit = std::mem::take(&mut m.edit);
            let footer_text = footer(m.tours.len());
            egui::ScrollArea::vertical().show(ui, |ui| {
                for t in &m.tours {
                    row_ui(ui, &m, t, &mut edit, &today, now, &mut clicks);
                }
                ui.add_space(4.0);
                ui.label(RichText::new(footer_text).color(theme::MUTED).size(12.0));
            });
            m.edit = edit;
        }
    }

    for click in clicks {
        match click {
            Click::RecordTour => out.go_to = Some(Page::Home),
            Click::Workspace => out.go_to = Some(Page::Workspace),
            Click::Refresh => {
                if let Some(load) = m.refresh() {
                    state.spawn_load(load);
                }
            }
            Click::Button(id, button, link) => match button {
                RowButton::CopyLink => {
                    if let Some(link) = link {
                        ui.ctx().copy_text(link);
                        m.copied(&id, now);
                    }
                }
                RowButton::Open => {
                    if let Some(link) = link
                        && let Err(e) = mythicshot_destinations::open_url(&link)
                    {
                        m.errors.insert(id, format!("Could not open the link: {e}."));
                    }
                }
                RowButton::Edit => {
                    if let Some(key) = m.ask_edit(&id) {
                        out.edit_project = Some(key);
                    }
                }
                RowButton::Rename => m.start_rename(&id),
                RowButton::Delete => m.ask_delete(&id),
                RowButton::Publish => {
                    if let Some(job) = m.request_publish(&id) {
                        state.spawn_job(job);
                    }
                }
                RowButton::PublishChanges => {
                    if let Some(job) = m.start_publish_changes(&id) {
                        state.spawn_job(job);
                    }
                }
                RowButton::Unpublish => {
                    if let Some(job) = m.start_unpublish(&id) {
                        state.spawn_job(job);
                    }
                }
            },
            Click::SaveRename => {
                if let Some(job) = m.save_rename() {
                    state.spawn_job(job);
                }
            }
            Click::CancelEdit => m.cancel_edit(),
            Click::CancelOpen(id) => m.cancel_open(&id),
            Click::ChooseChapter(chapter) => {
                if let Some(key) = m.choose_chapter(&chapter) {
                    out.edit_project = Some(key);
                }
            }
            Click::PublishForAnyone(id) => {
                if let Some(job) = m.confirm_publish(&id) {
                    state.spawn_job(job);
                }
            }
            Click::DeleteTour(id) => {
                if let Some(job) = m.confirm_delete(&id) {
                    state.spawn_job(job);
                }
            }
            Click::FinishDelete(entry) => {
                if let Some(job) = m.finish_delete(&entry) {
                    state.spawn_job(job);
                }
            }
            Click::DismissNotice => m.dismiss_notice(),
        }
    }

    // Jobs report into the model, not to the window: look again soon while one runs.
    let waiting = m.is_loading() || m.saved_reading || !m.working.is_empty() || m.opening.is_some() || conn.token == TokenStatus::Checking || m.copied.is_some();
    ui.ctx().request_repaint_after(if waiting { Duration::from_millis(100) } else { STALE_AFTER });
}

fn notice_ui(ui: &mut egui::Ui, notice: &Notice, clicks: &mut Vec<Click>) {
    if notice.error { theme::danger_frame() } else { theme::card_frame().stroke(Stroke::new(1.0, theme::BORDER)).inner_margin(9.0) }.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(&notice.text).color(if notice.error { theme::DANGER_TEXT } else { theme::MUTED })).wrap());
        if ui.button("Dismiss").clicked() {
            clicks.push(Click::DismissNotice);
        }
    });
    ui.add_space(8.0);
}

/// The block of one unfinished delete: what is left, and the button that carries on, or its progress.
fn unfinished_ui(ui: &mut egui::Ui, entry: &UnfinishedDelete, work: Option<Work>, clicks: &mut Vec<Click>) {
    theme::danger_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(unfinished_sentence(&entry.title)).color(theme::DANGER_TEXT)).wrap());
        match work {
            Some(work) => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new(work_message(work)).color(theme::MUTED));
                });
            }
            None => {
                if ui.button(FINISH_DELETING).clicked() {
                    clicks.push(Click::FinishDelete(entry.clone()));
                }
            }
        }
    });
    ui.add_space(8.0);
}

/// One tour: its title and badge, the detail line, and its buttons, or what is running on it.
fn row_ui(ui: &mut egui::Ui, m: &Model, t: &RemoteTour, edit: &mut Edit, today: &DateTime<Local>, now: Instant, clicks: &mut Vec<Click>) {
    let renaming = matches!(edit, Edit::Renaming { id, .. } if *id == t.id);
    let confirming = matches!(edit, Edit::ConfirmDelete { id } if *id == t.id);
    let choosing = matches!(edit, Edit::ChoosingChapter { id } if *id == t.id);
    let asking_publish = matches!(edit, Edit::ConfirmPublish { id } if *id == t.id);
    let saved = m.saved(&t.id);
    theme::card_frame().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if renaming {
            if let Edit::Renaming { text, .. } = edit {
                ui.horizontal(|ui| {
                    let field = ui.add(egui::TextEdit::singleline(text).char_limit(TITLE_MAX).desired_width((ui.available_width() - 150.0).max(120.0)));
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    // The field is what the keyboard goes to while the rename is open.
                    if !enter && ui.memory(|m| m.focused().is_none()) {
                        field.request_focus();
                    }
                    let can_save = !text.trim().is_empty();
                    if (ui.add_enabled(can_save, theme::primary_button("Save")).clicked() || enter) && can_save {
                        clicks.push(Click::SaveRename);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        clicks.push(Click::CancelEdit);
                    }
                });
                ui.label(RichText::new(RENAME_NOTE).color(theme::MUTED).size(12.0));
            }
        } else {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    badge_ui(ui, t);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&t.title).size(theme::STRONG_SIZE).bold()).truncate()).on_hover_text(&t.title);
                    });
                });
            });
        }
        ui.add(egui::Label::new(RichText::new(detail_line(t, saved, m.converting(), today)).color(theme::MUTED).size(12.0)).wrap());
        ui.add_space(4.0);
        if let Some(work) = m.work(&t.id) {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new(work_message(work)).color(theme::MUTED));
                // Expanding a long recording takes minutes: it can be stopped.
                if matches!(work, Work::Opening(_)) && ui.button("Cancel").clicked() {
                    clicks.push(Click::CancelOpen(t.id.clone()));
                }
            });
        } else if confirming {
            theme::danger_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(delete_question(t, saved.len())).color(theme::DANGER_TEXT)).wrap());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep it").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        clicks.push(Click::CancelEdit);
                    }
                    if ui.add(theme::danger_button("Delete tour")).clicked() {
                        clicks.push(Click::DeleteTour(t.id.clone()));
                    }
                });
            });
        } else if asking_publish {
            // Cancel is the default: Enter and Escape both choose it.
            theme::danger_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(crate::share_ui::who::CONFIRM_HEADING).bold()).wrap());
                ui.add(egui::Label::new(RichText::new(crate::share_ui::who::CONFIRM_TEXT).color(theme::MUTED)).wrap());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let keys = ui.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter));
                    for (label, default) in confirm_publish_buttons() {
                        let button = if default { theme::primary_button(label) } else { egui::Button::new(label) };
                        let clicked = ui.add(button).clicked();
                        if (clicked || (default && keys)) && default {
                            clicks.push(Click::CancelEdit);
                        } else if clicked {
                            clicks.push(Click::PublishForAnyone(t.id.clone()));
                        }
                    }
                });
            });
        } else if choosing {
            ui.label(RichText::new(WHICH_CHAPTER).color(theme::MUTED));
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                for p in usable(saved) {
                    if ui.button(chapter_label(p)).clicked() {
                        clicks.push(Click::ChooseChapter(p.key.chapter_id.clone()));
                    }
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    clicks.push(Click::CancelEdit);
                }
            });
        } else if !renaming {
            // Wrapped: with Publish changes a public row has seven buttons, more than a narrow window holds on one line.
            ui.horizontal_wrapped(|ui| {
                for b in row_buttons(t) {
                    let text = if b == RowButton::Delete { RichText::new(b.label()).color(theme::DANGER_TEXT) } else { RichText::new(b.label()) };
                    if ui.button(text).clicked() {
                        clicks.push(Click::Button(t.id.clone(), b, t.link.clone()));
                    }
                }
                if m.shows_copied(&t.id, now) {
                    ui.label(RichText::new(LINK_COPIED).color(theme::MUTED));
                }
            });
        }
        if let Some(e) = m.error(&t.id) {
            ui.add(egui::Label::new(RichText::new(e).color(theme::DANGER_TEXT).size(12.0)).wrap());
        }
    });
    ui.add_space(6.0);
}

fn badge_ui(ui: &mut egui::Ui, t: &RemoteTour) {
    let (fg, bg) = if t.is_public() { (theme::SUCCESS_TEXT, theme::SUCCESS_TINT) } else { (theme::MUTED, theme::CHIP) };
    egui::Frame::new().fill(bg).corner_radius(CornerRadius::same(9)).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
        ui.label(RichText::new(badge(t)).color(fg).size(theme::SMALL_SIZE));
    });
}

/// The Refresh button: a circular arrow, drawn here.
fn refresh_button(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::RAISED);
    }
    let color = if enabled { theme::TEXT } else { theme::DISABLED };
    let (c, r) = (rect.center(), 7.0);
    let at = |deg: f32| c + vec2(deg.to_radians().cos(), deg.to_radians().sin()) * r;
    // Three quarters of a circle, with an arrow head where it ends.
    let arc: Vec<egui::Pos2> = (0..=27).map(|i| at(-60.0 + i as f32 * 10.0)).collect();
    p.add(egui::Shape::line(arc, Stroke::new(1.6, color)));
    let tip = at(-60.0);
    p.add(egui::Shape::convex_polygon(vec![tip + vec2(2.5, -4.5), tip + vec2(3.5, 2.5), tip + vec2(-3.5, 0.5)], color, Stroke::NONE));
    resp.on_hover_text("Refresh")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::publish::tests::{Answer, Stub, TOKEN, reply, stub};
    use chrono::Utc;

    const A: &str = "tour_aaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "tour_bbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "tour_cccccccccccccccccccccccc";

    fn tour(id: &str, title: &str, public: bool, steps: u32) -> RemoteTour {
        RemoteTour {
            id: id.into(),
            title: title.into(),
            status: if public { "published" } else { "draft" }.into(),
            chapters: 2,
            steps,
            link: public.then(|| format!("https://tours.example.com/t/{}", &id[5..11])),
            updated_at: Some(1_763_424_000_000),
            published_at: public.then_some(1_763_467_200_000),
            unpublished_changes: false,
        }
    }

    fn site() -> Site {
        Site { server: "https://tours.example.com".into(), epoch: 1 }
    }

    fn conn(token: TokenStatus) -> Conn {
        Conn { server: "https://tours.example.com".into(), token, epoch: 1, bound: Some("https://tours.example.com".into()) }
    }

    /// A model with a loaded list.
    fn loaded(tours: Vec<RemoteTour>) -> Model {
        let mut m = Model::default();
        m.set_site(Some(site()));
        let (generation, _) = m.begin_load().unwrap();
        m.finish_load(generation, Ok(tours), Instant::now());
        m
    }

    fn two() -> Vec<RemoteTour> {
        vec![tour(A, "Customer onboarding", true, 22), tour(B, "Testing", false, 9)]
    }

    /// Read the list again with this answer.
    fn reload(m: &mut Model, tours: Vec<RemoteTour>) {
        let (generation, _) = m.begin_load().expect("a load can start");
        m.finish_load(generation, Ok(tours), Instant::now());
    }

    #[test]
    fn the_detail_line_counts_and_dates_as_the_mockup_does() {
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap();
        let mut t = tour(A, "Customer onboarding", true, 22);
        // 18 Nov 2026 12:00 UTC.
        t.published_at = Some(now.timestamp_millis());
        assert_eq!(detail_line(&t, &[], None, &now), "2 chapters · 22 steps · published 18 Nov");
        let mut d = tour(B, "Testing", false, 9);
        d.chapters = 1;
        d.updated_at = Some(Utc.with_ymd_and_hms(2026, 11, 3, 8, 0, 0).unwrap().timestamp_millis());
        assert_eq!(detail_line(&d, &[], None, &now), "1 chapter · 9 steps · updated 3 Nov");
        d.steps = 1;
        assert_eq!(detail_line(&d, &[], None, &now), "1 chapter · 1 step · updated 3 Nov");
        // Another year is named.
        t.published_at = Some(Utc.with_ymd_and_hms(2025, 12, 31, 8, 0, 0).unwrap().timestamp_millis());
        assert_eq!(detail_line(&t, &[], None, &now), "2 chapters · 22 steps · published 31 Dec 2025");
        // An older site gives no publish date: the line says nothing it does not know.
        t.published_at = None;
        assert_eq!(detail_line(&t, &[], None, &now), "2 chapters · 22 steps");
        d.updated_at = None;
        assert_eq!(detail_line(&d, &[], None, &now), "1 chapter · 1 step");
        // A time no calendar has.
        d.updated_at = Some(i64::MAX);
        assert_eq!(detail_line(&d, &[], None, &now), "1 chapter · 1 step");
        // The day is the viewer's own: 23:30 UTC is already the 19th two hours east.
        let east = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 11, 18, 23, 30, 0).unwrap().timestamp_millis();
        assert_eq!(day_label(late, &now.with_timezone(&east)).as_deref(), Some("19 Nov"));
        assert_eq!(day_label(late, &now).as_deref(), Some("18 Nov"));
    }

    #[test]
    fn a_public_tour_and_a_draft_show_their_own_buttons() {
        use RowButton::*;
        let public = tour(A, "A", true, 3);
        assert_eq!(row_buttons(&public), [CopyLink, Open, Edit, Rename, Unpublish, Delete]);
        assert_eq!(row_buttons(&public).iter().map(|b| b.label()).collect::<Vec<_>>(), ["Copy link", "Open", "Edit", "Rename", "Unpublish", "Delete"]);
        let draft = tour(B, "B", false, 3);
        assert_eq!(row_buttons(&draft), [Publish, Edit, Rename, Delete]);
        assert_eq!(row_buttons(&draft).iter().map(|b| b.label()).collect::<Vec<_>>(), ["Publish", "Edit", "Rename", "Delete"]);
        assert_eq!((badge(&public), badge(&draft)), ("Public", "Draft"));
        // An older site names no link: nothing to copy or open, the rest stays.
        let mut unlinked = public.clone();
        unlinked.link = None;
        assert_eq!(row_buttons(&unlinked), [Edit, Rename, Unpublish, Delete]);
        // A draft never offers a link, whatever the row holds.
        let mut odd = draft.clone();
        odd.link = Some("https://tours.example.com/t/x".into());
        assert_eq!(row_buttons(&odd), [Publish, Edit, Rename, Delete]);
    }

    #[test]
    fn the_delete_question_names_the_tour_and_its_steps() {
        assert_eq!(delete_question(&tour(B, "Testing", false, 9), 0), "Delete \"Testing\"? Its 9 steps and images are removed from the site. This can't be undone.");
        assert_eq!(delete_question(&tour(B, "One", false, 1), 0), "Delete \"One\"? Its 1 step and images are removed from the site. This can't be undone.");
        let long = delete_question(&tour(B, &"x".repeat(300), false, 2), 1);
        assert!(long.chars().count() < 220 && long.contains('…'), "{long}");
    }

    #[test]
    fn the_footer_counts_tours_and_progress_reads_as_a_sentence_start() {
        assert_eq!(footer(2), "2 tours in this workspace");
        assert_eq!(footer(1), "1 tour in this workspace");
        assert_eq!(work_message(Work::Publishing(Some((12, 22)))), "Publishing… 12 of 22 images");
        assert_eq!(work_message(Work::Publishing(None)), "Publishing…");
        assert_eq!(work_message(Work::Deleting(Some((250, 601)))), "Deleting… 250 of 601 files");
        assert_eq!(work_message(Work::Deleting(Some((0, 1)))), "Deleting… 0 of 1 file");
        assert_eq!(work_message(Work::Deleting(None)), "Deleting…");
        assert_eq!(work_message(Work::Unpublishing), "Unpublishing…");
        assert_eq!(work_message(Work::Renaming), "Saving the new name…");
        for s in [NOT_CONNECTED, EMPTY, RENAME_NOTE, LIST_CHANGED, publish::TOUR_GONE] {
            assert!(s.ends_with('.') && !s.contains('!'), "{s}");
        }
    }

    #[test]
    fn the_page_goes_from_not_connected_through_loading_to_the_list_or_a_failure() {
        let mut m = Model::default();
        assert_eq!(m.view(&conn(TokenStatus::Missing)), View::NotConnected(NOT_CONNECTED.into()));
        assert_eq!(m.view(&conn(TokenStatus::Checking)), View::Checking);
        assert_eq!(m.view(&conn(TokenStatus::Unavailable("The credential store is locked.".into()))), View::NotConnected("The credential store is locked.".into()));
        assert!(!m.wants_load(Instant::now()) && m.begin_load().is_none(), "nothing is asked of a site that is not set up");
        // A connection with a bad address or no token is no site.
        assert_eq!(Conn { server: "http://tours.example.com".into(), ..conn(TokenStatus::Saved) }.site(), None);
        assert_eq!(Conn { server: String::new(), ..conn(TokenStatus::Saved) }.site(), None);
        assert_eq!(conn(TokenStatus::Missing).site(), None);
        assert_eq!(conn(TokenStatus::Saved).site(), Some(site()));

        let saved = conn(TokenStatus::Saved);
        m.set_site(saved.site());
        assert_eq!(m.view(&saved), View::Loading);
        assert!(m.wants_load(Instant::now()));
        let (generation, to) = m.begin_load().unwrap();
        assert_eq!(to, site());
        assert!(m.is_loading() && !m.wants_load(Instant::now()) && m.begin_load().is_none(), "one list request at a time");
        m.finish_load(generation, Err("Could not list the tours: the server did not answer in time.".into()), Instant::now());
        assert_eq!(m.view(&saved), View::Failed("Could not list the tours: the server did not answer in time.".into()));
        assert!(!m.wants_load(Instant::now()), "a failure waits for Retry instead of asking in a loop");
        // A token the site refused (revoked there) reads as not connected, with the way to the Workspace page.
        let (generation, _) = m.refresh().unwrap();
        m.finish_load(generation, Err(publish::REJECTED_TOKEN.into()), Instant::now());
        assert_eq!(m.view(&saved), View::NotConnected("The server rejected the API token. Sign in again on the Workspace page of the main window.".into()));
        assert!(!m.wants_load(Instant::now()), "and nothing is asked again on its own");
        // Retry.
        let (generation, _) = m.refresh().unwrap();
        m.finish_load(generation, Ok(vec![]), Instant::now());
        assert_eq!(m.view(&saved), View::Empty);
        reload(&mut m, two());
        assert_eq!(m.view(&saved), View::List);
        assert_eq!(m.tours().len(), 2);
    }

    #[test]
    fn an_old_list_is_read_again_but_not_under_an_edit_or_a_running_job() {
        let t0 = Instant::now();
        let mut m = loaded(two());
        assert!(!m.wants_load(t0));
        let later = t0 + STALE_AFTER + Duration::from_secs(1);
        assert!(m.wants_load(later));
        m.ask_delete(B);
        assert!(!m.wants_load(later), "a confirmation is not pulled away by a timer");
        m.cancel_edit();
        m.start_rename(A);
        assert!(!m.wants_load(later));
        m.cancel_edit();
        let job = m.start_unpublish(A).unwrap();
        assert!(!m.wants_load(later));
        m.finish(&job, Ok(Done::Unpublished));
        assert!(m.wants_load(t0), "after a job the list is read again at once");
    }

    #[test]
    fn an_answer_from_an_older_request_is_dropped() {
        let mut m = loaded(two());
        let (old, _) = m.begin_load().unwrap();
        // The site changes while the request is out.
        m.set_site(Some(Site { server: "https://other.example.com".into(), epoch: 1 }));
        m.finish_load(old, Ok(two()), Instant::now());
        assert!(m.tours().is_empty(), "the other site's tours are not shown");
        assert_eq!(m.view(&conn(TokenStatus::Saved)), View::Loading);
        // A job that finishes makes a request that was already out stale.
        let mut m = loaded(two());
        let job = m.start_unpublish(A).unwrap();
        let (old, _) = m.begin_load().unwrap();
        m.finish(&job, Ok(Done::Unpublished));
        m.finish_load(old, Ok(two()), Instant::now());
        assert!(!m.tours()[0].is_public(), "the list from before the unpublish does not bring the old state back");
        assert!(m.wants_load(Instant::now()));
    }

    #[test]
    fn rename_happens_in_place_and_saves_only_a_new_name() {
        let mut m = loaded(two());
        m.start_rename(B);
        assert_eq!(m.edit(), &Edit::Renaming { id: B.into(), text: "Testing".into() });
        // Save without a change closes the field and sends nothing.
        assert_eq!(m.save_rename(), None);
        assert_eq!(m.edit(), &Edit::None);
        // An empty name keeps the field open.
        m.start_rename(B);
        *m.rename_text().unwrap() = "   ".into();
        assert_eq!(m.save_rename(), None);
        assert!(matches!(m.edit(), Edit::Renaming { .. }));
        // Cancel.
        m.cancel_edit();
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!(m.rename_text(), None);
        // A new name is trimmed and sent for that tour.
        m.start_rename(B);
        *m.rename_text().unwrap() = "  Trial run ".into();
        let job = m.save_rename().unwrap();
        assert_eq!(job, Started { site: site(), id: B.into(), title: "Testing".into(), job: Job::Rename("Trial run".into()), listed: true });
        assert_eq!(m.work(B), Some(Work::Renaming));
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!(m.save_rename(), None, "a second Save has nothing to save");
        m.finish(&job, Ok(Done::Renamed("Trial run".into())));
        assert_eq!(m.tours()[1].title, "Trial run");
        assert_eq!(m.work(B), None);
        // A refused name shows on the row, and the title stays.
        m.start_rename(B);
        *m.rename_text().unwrap() = "Bad\u{7}".into();
        let job = m.save_rename().unwrap();
        m.finish(&job, Err(Failure::Message("Could not rename the tour: the title must not hold control characters (HTTP 400).".into())));
        assert_eq!(m.tours()[1].title, "Trial run");
        assert!(m.error(B).unwrap().starts_with("Could not rename the tour"));
        // Starting again clears the old error.
        m.start_rename(B);
        assert_eq!(m.error(B), None);
    }

    #[test]
    fn only_the_tour_whose_confirmation_shows_can_be_deleted() {
        let mut m = loaded(two());
        // Without a confirmation nothing is sent.
        assert_eq!(m.confirm_delete(B), None);
        m.ask_delete(B);
        assert_eq!(m.edit(), &Edit::ConfirmDelete { id: B.into() });
        // A "Delete tour" for any other tour is refused, and the confirmation stays where it is.
        assert_eq!(m.confirm_delete(A), None);
        assert_eq!(m.confirm_delete("tour_dddddddddddddddddddddddd"), None);
        assert_eq!(m.edit(), &Edit::ConfirmDelete { id: B.into() });
        // "Keep it" sends nothing.
        m.cancel_edit();
        assert_eq!(m.confirm_delete(B), None);
        assert!(m.work(B).is_none());
        // Asking about another tour moves the question, and only that one can be confirmed.
        m.ask_delete(B);
        m.ask_delete(A);
        assert_eq!(m.confirm_delete(B), None);
        let job = m.confirm_delete(A).unwrap();
        assert_eq!(job, Started { site: site(), id: A.into(), title: "Customer onboarding".into(), job: Job::Delete, listed: true });
        assert_eq!(m.work(A), Some(Work::Deleting(None)));
    }

    #[test]
    fn a_double_click_sends_one_delete() {
        let mut m = loaded(two());
        m.ask_delete(B);
        let first = m.confirm_delete(B);
        let second = m.confirm_delete(B);
        assert!(first.is_some());
        assert_eq!(second, None);
        // While it runs, nothing else starts on that tour, not even a new confirmation.
        m.ask_delete(B);
        assert_eq!(m.edit(), &Edit::None);
        m.start_rename(B);
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!((m.start_publish(B), m.start_unpublish(B), m.confirm_delete(B)), (None, None, None));
        // The other tour stays usable.
        assert!(m.start_unpublish(A).is_some());
        // The same holds for a double click on Publish and Unpublish.
        let mut m = loaded(two());
        assert!(m.start_publish(B).is_some() && m.start_publish(B).is_none());
        assert!(m.start_unpublish(A).is_some() && m.start_unpublish(A).is_none());
        // And each only fits the state the tour is in.
        let mut m = loaded(two());
        assert_eq!((m.start_publish(A), m.start_unpublish(B)), (None, None));
    }

    #[test]
    fn a_refresh_that_changes_the_list_closes_the_confirmation_and_never_moves_it() {
        let three = || vec![tour(A, "Customer onboarding", true, 22), tour(B, "Testing", false, 9), tour(C, "Billing", false, 4)];
        // The same list again: the question still stands.
        let mut m = loaded(three());
        m.ask_delete(B);
        reload(&mut m, three());
        assert_eq!(m.edit(), &Edit::ConfirmDelete { id: B.into() });
        assert_eq!(m.notice(), None);
        // The tour is gone.
        reload(&mut m, vec![three()[0].clone(), three()[2].clone()]);
        assert_eq!(m.edit(), &Edit::None, "it does not slide to the row that took its place");
        assert_eq!(m.notice().map(|n| n.text.as_str()), Some(LIST_CHANGED));
        assert_eq!(m.confirm_delete(B), None);
        assert_eq!(m.confirm_delete(C), None);
        // Reordered.
        let mut m = loaded(three());
        m.ask_delete(B);
        reload(&mut m, vec![three()[1].clone(), three()[0].clone(), three()[2].clone()]);
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!((m.confirm_delete(A), m.confirm_delete(B)), (None, None));
        // Another tour removed, or one added: the rows moved under the question.
        for other in [vec![three()[0].clone(), three()[1].clone()], [three(), vec![tour("tour_dddddddddddddddddddddddd", "New", false, 1)]].concat()] {
            let mut m = loaded(three());
            m.ask_delete(B);
            reload(&mut m, other);
            assert_eq!(m.edit(), &Edit::None);
        }
        // Renamed, or with other steps, elsewhere: the question no longer says what would be deleted.
        for change in [|t: &mut RemoteTour| t.title = "Production".into(), |t: &mut RemoteTour| t.steps = 40] {
            let mut m = loaded(three());
            m.ask_delete(B);
            let mut list = three();
            change(&mut list[1]);
            reload(&mut m, list);
            assert_eq!(m.edit(), &Edit::None);
            assert_eq!(m.confirm_delete(B), None);
        }
        // A list that cannot be read closes it too.
        let mut m = loaded(three());
        m.ask_delete(B);
        let (generation, _) = m.begin_load().unwrap();
        m.finish_load(generation, Err("Could not list the tours: the server did not answer in time.".into()), Instant::now());
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!(m.confirm_delete(B), None);
    }

    #[test]
    fn a_rename_in_progress_survives_a_refresh_unless_its_tour_is_gone() {
        let mut m = loaded(two());
        m.start_rename(B);
        *m.rename_text().unwrap() = "Half typed".into();
        reload(&mut m, vec![two()[1].clone(), two()[0].clone()]);
        assert_eq!(m.edit(), &Edit::Renaming { id: B.into(), text: "Half typed".into() }, "it is addressed by id, so a new order does not matter");
        reload(&mut m, vec![two()[0].clone()]);
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!(m.notice().map(|n| n.text.as_str()), Some(publish::TOUR_GONE));
        assert_eq!(m.save_rename(), None);
    }

    #[test]
    fn an_action_on_a_tour_that_vanished_says_so_and_reads_the_list_again() {
        for start in [|m: &mut Model| m.start_publish(B), |m: &mut Model| {
            m.start_rename(B);
            *m.rename_text().unwrap() = "New".into();
            m.save_rename()
        }] {
            let mut m = loaded(two());
            let job = start(&mut m).unwrap();
            m.finish(&job, Err(Failure::Gone));
            assert_eq!(m.notice(), Some(&Notice { text: publish::TOUR_GONE.into(), error: false }));
            assert_eq!(m.tours().iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), [A], "the row is gone");
            assert!(m.wants_load(Instant::now()));
            assert_eq!(m.error(B), None);
        }
        let mut m = loaded(two());
        let job = m.start_unpublish(A).unwrap();
        m.finish(&job, Err(failure("Could not unpublish the tour: that tour no longer exists.".into())));
        assert_eq!(m.notice().map(|n| n.text.as_str()), Some(publish::TOUR_GONE));
        // Clicking a row that a refresh removed in the same moment does nothing at all.
        let mut m = loaded(two());
        reload(&mut m, vec![two()[0].clone()]);
        m.ask_delete(B);
        m.start_rename(B);
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!((m.start_publish(B), m.confirm_delete(B)), (None, None));
    }

    #[test]
    fn what_a_job_did_shows_at_once() {
        let mut m = loaded(two());
        let job = m.start_publish(B).unwrap();
        m.progress(B, Work::Publishing(Some((3, 9))));
        assert_eq!(m.work(B), Some(Work::Publishing(Some((3, 9)))));
        m.finish(&job, Ok(Done::Published(Some("https://tours.example.com/t/bbb".into()))));
        assert!(m.tours()[1].is_public());
        assert_eq!(row_buttons(&m.tours()[1])[0], RowButton::CopyLink);
        let job = m.start_unpublish(B).unwrap();
        m.finish(&job, Ok(Done::Unpublished));
        assert!(!m.tours()[1].is_public() && m.tours()[1].link.is_none());
        m.ask_delete(B);
        let job = m.confirm_delete(B).unwrap();
        m.progress(B, Work::Deleting(Some((250, 601))));
        m.finish(&job, Ok(Done::Deleted));
        assert_eq!(m.tours().len(), 1);
        assert_eq!(m.notice(), None);
        // Progress for a job that is not running is ignored.
        m.progress(B, Work::Deleting(None));
        assert_eq!(m.work(B), None);
        // A failure stays on the row, and the row keeps its buttons.
        let job = m.start_unpublish(A).unwrap();
        m.finish(&job, Err(Failure::Message("Could not unpublish the tour: the server answered HTTP 503.".into())));
        assert_eq!(m.error(A), Some("Could not unpublish the tour: the server answered HTTP 503."));
        assert!(m.tours()[0].is_public() && m.work(A).is_none());
        // Refresh clears old messages.
        let (generation, _) = m.refresh().unwrap();
        m.finish_load(generation, Ok(two()), Instant::now());
        assert_eq!(m.error(A), None);
    }

    fn entry(server: &str, id: &str, title: &str) -> UnfinishedDelete {
        UnfinishedDelete { server: server.into(), tour_id: id.into(), title: title.into() }
    }

    #[test]
    fn a_delete_that_left_files_behind_can_be_carried_on_from_its_own_block() {
        let mut m = loaded(two());
        let here = entry(&site().server, B, "Testing");
        assert!(!m.shows_unfinished(&here), "while its row is in the list, the row speaks for it");
        assert_eq!(m.finish_delete(&here), None, "a tour that is in the list is deleted through its confirmation only");
        m.ask_delete(B);
        let job = m.confirm_delete(B).unwrap();
        m.finish(&job, Err(Failure::DeleteUnfinished("Could not delete the tour completely: the site stopped making progress at 250 of 601 files.".into())));
        assert_eq!(m.tours().len(), 1, "the tour itself is gone");
        assert!(m.notice().unwrap().error);
        assert!(m.shows_unfinished(&here));
        let again = m.finish_delete(&here).unwrap();
        assert_eq!(again, Started { site: site(), id: B.into(), title: "Testing".into(), job: Job::Delete, listed: false });
        assert_eq!(m.finish_delete(&here), None, "a second click sends nothing");
        assert_eq!(m.work(B), Some(Work::Deleting(None)));
        m.finish(&again, Ok(Done::Deleted));
        assert_eq!(m.work(B), None);
        // Only this site's entries, and only ones that name a tour.
        assert_eq!(m.finish_delete(&entry("https://other.example.com", C, "Elsewhere")), None);
        assert!(!m.shows_unfinished(&entry("https://other.example.com", C, "Elsewhere")));
        assert_eq!(m.finish_delete(&entry(&site().server, "../uploads/x", "Odd")), None);
        // Not connected: nothing can be sent.
        m.set_site(None);
        assert_eq!(m.finish_delete(&here), None);
        assert!(!m.shows_unfinished(&here));
    }

    #[test]
    fn nothing_the_page_does_takes_an_unfinished_delete_away() {
        let mut m = loaded(two());
        let here = entry(&site().server, B, "Testing");
        m.ask_delete(B);
        let job = m.confirm_delete(B).unwrap();
        m.finish(&job, Err(Failure::DeleteUnfinished("Could not delete the tour completely.".into())));
        // Refresh, which drops messages.
        let (generation, _) = m.refresh().unwrap();
        m.finish_load(generation, Ok(vec![two()[0].clone()]), Instant::now());
        assert_eq!(m.notice(), None);
        assert!(m.shows_unfinished(&here));
        // Other notices taking the one slot.
        let other = m.start_unpublish(A).unwrap();
        m.finish(&other, Err(Failure::Gone));
        assert_eq!(m.notice().map(|n| n.text.as_str()), Some(publish::TOUR_GONE));
        assert!(m.shows_unfinished(&here));
        m.dismiss_notice();
        assert!(m.shows_unfinished(&here));
        // A list that cannot be read.
        let (generation, _) = m.refresh().unwrap();
        m.finish_load(generation, Err("Could not list the tours: the server answered HTTP 503.".into()), Instant::now());
        assert!(m.shows_unfinished(&here));
        // Another site, then this one again (a token saved anew counts as that).
        m.set_site(Some(Site { server: "https://other.example.com".into(), epoch: 1 }));
        assert!(!m.shows_unfinished(&here));
        m.set_site(Some(Site { epoch: 2, ..site() }));
        assert!(m.shows_unfinished(&here));
        assert!(m.finish_delete(&here).is_some());
    }

    /// A list whose notes reach the settings file.
    fn saved_list() -> Unfinished {
        let u = Unfinished::default();
        u.save_with(|_| true);
        u
    }

    #[test]
    fn a_note_that_cannot_be_written_is_not_kept_in_memory_either() {
        // No way to write at all (the settings file is not to be touched this session), and a write that fails.
        let refusing = Unfinished::default();
        refusing.save_with(|_| false);
        for u in [Unfinished::default(), refusing] {
            assert_eq!(u.add("https://tours.example.com", B, "Testing"), Noted::NotSaved);
            assert!(u.for_site("https://tours.example.com").is_empty() && !u.is_changed());
            // A note read from the file at start-up is there already, and needs no write.
            u.load(vec![entry("https://tours.example.com", A, "Earlier")]);
            assert_eq!(u.add("https://tours.example.com", A, "Earlier"), Noted::Before);
            assert_eq!(u.add("https://tours.example.com", B, "Testing"), Noted::NotSaved);
            assert_eq!(u.for_site("https://tours.example.com"), [entry("https://tours.example.com", A, "Earlier")]);
            assert!(!u.is_changed(), "nothing for the app to write");
        }
        // A full list loses its oldest note only to a note that was written.
        let u = saved_list();
        let asked: Arc<Mutex<Vec<usize>>> = Arc::default();
        for i in 0..MAX_UNFINISHED {
            assert_eq!(u.add("https://tours.example.com", &format!("tour_{i:024x}"), "x"), Noted::Now);
        }
        let full = u.take_changed().unwrap();
        let seen = asked.clone();
        u.save_with(move |list| {
            seen.lock().unwrap().push(list.len());
            false
        });
        assert_eq!(u.add("https://tours.example.com", B, "Testing"), Noted::NotSaved);
        assert_eq!(*asked.lock().unwrap(), [MAX_UNFINISHED], "the write was tried, with the list as it would have been");
        assert_eq!(u.for_site("https://tours.example.com"), full);
        assert!(!u.is_changed());
        // Once the file can be written again, the same delete is noted.
        u.save_with(|_| true);
        assert_eq!(u.add("https://tours.example.com", B, "Testing"), Noted::Now);
        assert_eq!((u.add("https://tours.example.com", B, "Testing"), u.is_changed()), (Noted::Before, true));
    }

    #[test]
    fn a_change_whose_save_failed_is_handed_out_again_until_a_save_works() {
        let u = saved_list();
        assert!(u.settle(|list| list.is_none()), "nothing changed, and the save's own answer is passed on");
        u.add("https://tours.example.com", B, "Testing");
        // The settings file could not be written: the list is still to be written.
        assert!(!u.settle(|list| {
            assert_eq!(list, Some(vec![entry("https://tours.example.com", B, "Testing")]));
            false
        }));
        assert!(u.is_changed());
        // A delete finished meanwhile; the next save gets the list as it is now, and works.
        u.remove("https://tours.example.com", B);
        assert!(!u.settle(|list| list != Some(vec![])));
        assert!(u.is_changed());
        assert!(u.settle(|list| list == Some(vec![])));
        assert!(!u.is_changed());
        assert_eq!(u.take_changed(), None, "written once it worked");
        // A save that fails with nothing new in the list leaves nothing of the list to write.
        assert!(!u.settle(|list| list.is_some()));
        assert!(!u.is_changed());
    }

    #[test]
    fn a_settings_file_that_is_never_written_over_refuses_every_new_delete() {
        use crate::settings::{LoadProblem, Settings};
        let earlier = entry("https://tours.example.com", A, "Earlier");
        // The file could not be read at start-up and no copy was made: the app starts the list with no way to write.
        let mut untouched = Settings::default();
        untouched.load_problem = Some(LoadProblem { path: "settings.toml".into(), backup: None, partly_read: false });
        untouched.unfinished_deletes = vec![earlier.clone()];
        let u = Unfinished::default();
        u.start(untouched.unfinished_deletes.clone(), untouched.note_saver());
        assert_eq!(u.add("https://tours.example.com", B, "Testing"), Noted::NotSaved);
        assert_eq!(u.add("https://tours.example.com", A, "Earlier"), Noted::Before, "what it read can still be finished");
        assert_eq!(u.for_site("https://tours.example.com"), [earlier.clone()]);
        assert!(!u.is_changed());
        // A file that may be saved gets a way to write, and with it the note is made.
        assert!(Settings::default().note_saver().is_some());
        let u = Unfinished::default();
        let written: Arc<Mutex<Vec<UnfinishedDelete>>> = Arc::default();
        let disk = written.clone();
        u.start(vec![earlier.clone()], Some(move |list: &[UnfinishedDelete]| {
            *disk.lock().unwrap() = list.to_vec();
            true
        }));
        assert_eq!(u.add("https://tours.example.com", B, "Testing"), Noted::Now);
        assert_eq!(*written.lock().unwrap(), [earlier, entry("https://tours.example.com", B, "Testing")]);
    }

    #[test]
    fn the_unfinished_list_keeps_every_site_and_shows_one() {
        let u = saved_list();
        assert_eq!(u.take_changed(), None);
        u.add("https://tours.example.com", B, "Testing");
        assert!(u.is_changed());
        // The same tour twice is one entry.
        u.add("https://tours.example.com", B, "Testing");
        u.add("https://other.example.com", C, "Elsewhere");
        u.add("https://tours.example.com", A, "  ");
        assert_eq!(u.for_site("https://tours.example.com"), [entry("https://tours.example.com", B, "Testing"), entry("https://tours.example.com", A, "Untitled tour")]);
        assert_eq!(u.for_site("https://other.example.com"), [entry("https://other.example.com", C, "Elsewhere")]);
        assert!(u.for_site("https://third.example.com").is_empty());
        assert_eq!(u.take_changed().map(|l| l.len()), Some(3));
        assert_eq!(u.take_changed(), None, "a change is written once");
        // Removing what is not there changes nothing; removing one site's entry leaves the other's.
        u.remove("https://other.example.com", B);
        assert!(!u.is_changed());
        u.remove("https://tours.example.com", B);
        assert_eq!(u.take_changed(), Some(vec![entry("https://other.example.com", C, "Elsewhere"), entry("https://tours.example.com", A, "Untitled tour")]));
        // Something in the file that is not a tour id is never offered.
        u.load(vec![entry("https://tours.example.com", "../uploads/x", "Odd"), entry("https://tours.example.com", B, "Testing")]);
        assert!(!u.is_changed(), "what was read from the file is not a change");
        assert_eq!(u.for_site("https://tours.example.com"), [entry("https://tours.example.com", B, "Testing")]);
        // The list cannot grow without end, and a title is kept short.
        let u = saved_list();
        for i in 0..MAX_UNFINISHED + 5 {
            u.add("https://tours.example.com", &format!("tour_{i:024x}"), &"x".repeat(500));
        }
        let all = u.take_changed().unwrap();
        assert_eq!(all.len(), MAX_UNFINISHED);
        assert_eq!(all[0].tour_id, format!("tour_{:024x}", 5));
        assert_eq!(all[0].title.chars().count(), TITLE_MAX);
    }

    #[test]
    fn unfinished_deletes_survive_a_restart_through_the_settings_file() {
        let u = saved_list();
        u.add("https://tours.example.com", B, "Testing \"quoted\" · café");
        u.add("https://other.example.com", C, "Elsewhere");
        // What the app does with a change: into the settings, and the settings to disk.
        let mut settings = crate::settings::Settings::default();
        settings.unfinished_deletes = u.take_changed().unwrap();
        let text = toml::to_string_pretty(&settings).unwrap();
        assert!(!text.to_ascii_lowercase().contains("token") && !text.contains("dp_"), "nothing secret is written:\n{text}");
        // The next start.
        let read: crate::settings::Settings = toml::from_str(&text).unwrap();
        let after = Unfinished::default();
        after.load(read.unfinished_deletes);
        assert_eq!(after.for_site("https://tours.example.com"), [entry("https://tours.example.com", B, "Testing \"quoted\" · café")]);
        assert_eq!(after.for_site("https://other.example.com").len(), 1, "the other site's entry is kept for when that site is configured");
        assert!(!after.is_changed());
        // A settings file from before this existed has none.
        let old: crate::settings::Settings = toml::from_str("publish_server = \"https://tours.example.com\"\n").unwrap();
        assert!(old.unfinished_deletes.is_empty() && old.publish_saved_for.is_none());
        assert_eq!(unfinished_sentence("Testing"), "Deleting \"Testing\" did not finish. Some of its files may still be on the site.");
    }

    #[test]
    fn a_token_saved_for_another_address_is_no_connection() {
        let mut c = conn(TokenStatus::Saved);
        assert_eq!((c.binding_problem(), c.site()), (None, Some(site())));
        // The address was changed (a typo, a new host): the token stays home.
        c.server = "https://tours.example.co".into();
        assert_eq!(c.site(), None);
        let m = Model::default();
        let View::NotConnected(why) = m.view(&c) else { panic!("connected") };
        assert_eq!(why, format!("The saved token belongs to tours.example.com, so it is not sent to this address. {}", publish_ui::FIX_ON_WORKSPACE));
        // A token tied to no address yet.
        let unbound = Conn { bound: None, ..conn(TokenStatus::Saved) };
        assert_eq!(unbound.site(), None);
        assert!(matches!(m.view(&unbound), View::NotConnected(why) if why.starts_with("The saved token is not tied to a site address yet")));
        // The same address written another way is the same site.
        let same = Conn { server: " https://Tours.Example.com/ ".into(), ..conn(TokenStatus::Saved) };
        assert_eq!(same.site(), Some(site()));
        // Without a token the plain sentence stands.
        assert_eq!(m.view(&Conn { server: "https://tours.example.co".into(), ..conn(TokenStatus::Missing) }), View::NotConnected(NOT_CONNECTED.into()));
    }

    #[test]
    fn forgetting_the_token_empties_the_page_and_lets_a_running_job_finish_quietly() {
        let mut m = loaded(two());
        m.ask_delete(B);
        let job = m.confirm_delete(B).unwrap();
        let other = m.start_unpublish(A).unwrap();
        // Forget: no token, so no site.
        m.set_site(conn(TokenStatus::Missing).site());
        assert_eq!(m.view(&conn(TokenStatus::Missing)), View::NotConnected(NOT_CONNECTED.into()));
        assert!(m.tours().is_empty() && m.edit() == &Edit::None);
        assert_eq!((m.start_publish(B), m.confirm_delete(B), m.begin_load()), (None, None, None));
        // The jobs end on their own; nothing of them shows on a page that is not connected.
        m.finish(&job, Ok(Done::Deleted));
        m.finish(&other, Err(Failure::Message("Could not unpublish the tour: the server answered HTTP 503.".into())));
        assert_eq!((m.notice(), m.work(A), m.work(B)), (None, None, None));
        assert!(!m.wants_load(Instant::now()));
        // The same token saved again is a new connection: the list is read afresh.
        m.set_site(Some(Site { epoch: 2, ..site() }));
        assert!(m.wants_load(Instant::now()));
        // A job started for the old connection still does not write into the new one.
        reload(&mut m, two());
        m.finish(&job, Ok(Done::Deleted));
        assert_eq!(m.tours().len(), 2);
    }

    #[test]
    fn link_copied_shows_briefly_beside_the_tour_it_was_copied_from() {
        let t0 = Instant::now();
        let mut m = loaded(two());
        assert!(!m.shows_copied(A, t0));
        m.copied(A, t0);
        assert!(m.shows_copied(A, t0 + Duration::from_millis(500)));
        assert!(!m.shows_copied(B, t0));
        assert!(!m.shows_copied(A, t0 + COPIED_FOR));
    }

    // ---- With a site behind it ----

    const LIST: &str = r#"{"tours":[
        {"id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","title":"Customer onboarding","status":"published","slug":"abc123","url":"/t/abc123","chapters":2,"steps":22,"updated_at":1763424000000,"published_at":1763467200000},
        {"id":"tour_bbbbbbbbbbbbbbbbbbbbbbbb","title":"Testing","status":"draft","slug":"def456","url":null,"chapters":1,"steps":9,"updated_at":1763424000000,"published_at":null}]}"#;
    const ONLY_A: &str = r#"{"tours":[{"id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","title":"Customer onboarding","status":"published","slug":"abc123","url":"/t/abc123","chapters":2,"steps":22,"updated_at":1763424000000,"published_at":1763467200000}]}"#;
    const NO_TOUR: &str = r#"{"error":"not_found","detail":"no such tour"}"#;

    /// A page whose settings file can be written.
    fn page() -> ToursState {
        let state = unsaved_page();
        state.unfinished().save_with(|_| true);
        state
    }

    /// A page as it is when the settings file is not to be written this session.
    fn unsaved_page() -> ToursState {
        ToursState::with_token(|_| Ok(TOKEN.to_string()))
    }

    fn stub_conn(stub: &Stub) -> Conn {
        Conn { server: stub.base.clone(), token: TokenStatus::Saved, epoch: 1, bound: Some(stub.base.clone()) }
    }

    /// Every piece of text one frame of the page draws.
    fn drawn(ctx: &egui::Context, state: &mut ToursState, conn: &Conn) -> (String, PageOut) {
        fn collect(shape: &egui::Shape, into: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => into.push(t.galley.text().to_string()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, into)),
                _ => {}
            }
        }
        let mut page_out = PageOut::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(590.0, 520.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| super::ui(ui, state, conn, &mut page_out));
        out.textures_delta.clear();
        let mut texts = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        (texts.join("\n"), page_out)
    }

    /// Draw frames until `done` says the page has settled.
    fn until(ctx: &egui::Context, state: &mut ToursState, conn: &Conn, done: impl Fn(&Model, &str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (text, _) = drawn(ctx, state, conn);
            if done(&lock(&state.model), &text) {
                // The model may have moved on right after that frame was drawn: draw what it holds now.
                return drawn(ctx, state, conn).0;
            }
            assert!(Instant::now() < deadline, "the page did not settle:\n{text}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn has_line(text: &str, line: &str) -> bool {
        text.lines().any(|l| l == line)
    }

    #[test]
    fn the_page_draws_the_tours_with_their_badges_buttons_and_the_footer() {
        let s = stub(|req, _| if req.line == "GET /api/v1/tours" { reply(200, LIST) } else { reply(404, r#"{"error":"not_found"}"#) });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        let (first, _) = drawn(&ctx, &mut state, &conn);
        // The list is being read, unless the stub already answered.
        assert!(has_line(&first, "Tours") && has_line(&first, RECORD_A_TOUR) && (has_line(&first, LOADING) || has_line(&first, "Testing")), "{first}");
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading());
        for word in ["Tours", RECORD_A_TOUR, "Customer onboarding", "Public", "Testing", "Draft", "Copy link", "Open", "Rename", "Unpublish", "Delete", "Publish", "2 tours in this workspace"] {
            assert!(has_line(&text, word), "the page is missing {word:?} in:\n{text}");
        }
        assert!(text.contains("2 chapters · 22 steps · published ") && text.contains("1 chapter · 9 steps · updated "), "{text}");
        assert_eq!(text.lines().filter(|l| *l == "Rename").count(), 2);
        assert_eq!(text.lines().filter(|l| *l == "Delete").count(), 2);
        assert!(!text.contains("Keep it") && !text.contains(RENAME_NOTE));
        // Only the list was asked for, with the token in the header and nowhere else.
        let reqs = s.requests();
        assert!(reqs.iter().all(|r| r.line == "GET /api/v1/tours" && !r.line.contains("dp_")));
        assert!(!text.contains("dp_"));

        // Rename in place: the field, Save, Cancel and the note; the other row is untouched.
        lock(&state.model).start_rename(B);
        let (text, _) = drawn(&ctx, &mut state, &conn);
        for word in ["Save", "Cancel", RENAME_NOTE, "Customer onboarding", "Unpublish"] {
            assert!(has_line(&text, word), "renaming is missing {word:?} in:\n{text}");
        }
        assert!(!has_line(&text, "Publish"), "the row being renamed shows no other buttons:\n{text}");
        // The delete confirmation, in place of that row's buttons.
        lock(&state.model).ask_delete(B);
        let (text, _) = drawn(&ctx, &mut state, &conn);
        let joined = text.replace('\n', " ");
        assert!(joined.contains("Delete \"Testing\"? Its 9 steps and images are removed from the site. This can't be undone."), "{text}");
        assert!(has_line(&text, "Keep it") && has_line(&text, "Delete tour") && !has_line(&text, "Publish"), "{text}");
        assert_eq!(text.lines().filter(|l| *l == "Delete").count(), 1, "the other tour keeps its own Delete");
        assert_eq!(s.lines().iter().filter(|l| l.starts_with("DELETE")).count(), 0, "asking sends nothing");
    }

    #[test]
    fn a_page_that_is_not_connected_empty_or_failing_says_so_in_a_sentence() {
        let ctx = crate::theme::test_context();
        let mut state = page();
        let (text, _) = drawn(&ctx, &mut state, &conn(TokenStatus::Missing));
        assert!(has_line(&text, NOT_CONNECTED) && has_line(&text, OPEN_WORKSPACE) && !text.contains("Retry"), "{text}");
        let (text, _) = drawn(&ctx, &mut state, &conn(TokenStatus::Checking));
        assert!(has_line(&text, CHECKING) && !has_line(&text, OPEN_WORKSPACE), "{text}");

        let s = stub(|_, _| reply(200, r#"{"tours":[]}"#));
        let (mut state, c) = (page(), stub_conn(&s));
        let text = until(&ctx, &mut state, &c, |m, _| !m.is_loading()).replace('\n', " ");
        assert!(text.contains(EMPTY) && !text.contains("in this workspace"), "{text}");

        // A site that fails: the sentence and Retry, and no request loop.
        let s = stub(|_, _| reply(503, ""));
        let (mut state, c) = (page(), stub_conn(&s));
        let text = until(&ctx, &mut state, &c, |m, _| !m.is_loading());
        assert!(text.replace('\n', " ").contains("Could not list the tours: the server answered HTTP 503.") && has_line(&text, "Retry"), "{text}");
        for _ in 0..5 {
            drawn(&ctx, &mut state, &c);
        }
        assert_eq!(s.lines().len(), 1);
        // A token that cannot be read is a sentence too, never a panic.
        let mut state = ToursState::with_token(|_| Err("Could not use the credential store: the credential store is locked or not reachable.".into()));
        let text = until(&ctx, &mut state, &c, |m, _| !m.is_loading());
        assert!(text.replace('\n', " ").contains("the credential store is locked") && has_line(&text, "Retry"), "{text}");
    }

    #[test]
    fn a_confirmed_delete_is_sent_for_that_tour_and_the_list_is_read_again() {
        let s = stub(|req, seen| match req.line.as_str() {
            "GET /api/v1/tours" => reply(200, if seen == 0 { LIST } else { ONLY_A }),
            "DELETE /api/v1/tours/tour_bbbbbbbbbbbbbbbbbbbbbbbb" => {
                if seen == 0 { reply(202, r#"{"status":"in_progress","done":5,"total":10}"#) } else { reply(200, r#"{"deleted":true}"#) }
            }
            _ => reply(500, r#"{"error":"unexpected"}"#),
        });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let job = {
            let mut m = lock(&state.model);
            m.ask_delete(B);
            // Two clicks in one frame.
            let job = m.confirm_delete(B);
            assert_eq!(m.confirm_delete(B), None);
            job.unwrap()
        };
        state.spawn_job(job);
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert!(text.contains("Deleting…") && has_line(&text, "Unpublish"), "the tour shows progress, the other stays usable:\n{text}");
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading() && s.lines().iter().filter(|l| l.starts_with("GET")).count() == 2);
        assert!(has_line(&text, "1 tour in this workspace") && !text.contains("Testing"), "{text}");
        let deletes: Vec<String> = s.lines().into_iter().filter(|l| l.starts_with("DELETE")).collect();
        assert_eq!(deletes, ["DELETE /api/v1/tours/tour_bbbbbbbbbbbbbbbbbbbbbbbb"; 2], "one delete loop, for the confirmed tour only");
    }

    #[test]
    fn publishing_a_tour_deleted_elsewhere_reports_it_and_refreshes() {
        let s = stub(|req, seen| match req.line.as_str() {
            "GET /api/v1/tours" => reply(200, if seen == 0 { LIST } else { ONLY_A }),
            _ => reply(404, NO_TOUR),
        });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let job = lock(&state.model).start_publish(B).unwrap();
        state.spawn_job(job);
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading() && s.lines().iter().filter(|l| l.starts_with("GET")).count() == 2);
        assert!(has_line(&text, publish::TOUR_GONE) && has_line(&text, "1 tour in this workspace"), "{text}");
    }

    #[test]
    fn a_job_finishes_after_the_window_is_gone_and_forget_does_not_stop_it() {
        // The delete answers slowly; meanwhile the page is dropped, as when the window closes.
        let s = stub(|req, _| match req.line.as_str() {
            "GET /api/v1/tours" => reply(200, LIST),
            l if l.starts_with("DELETE") => {
                std::thread::sleep(Duration::from_millis(300));
                reply(200, r#"{"deleted":true}"#)
            }
            _ => Answer::Drop,
        });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let job = {
            let mut m = lock(&state.model);
            m.ask_delete(B);
            m.confirm_delete(B).unwrap()
        };
        state.spawn_job(job);
        // Forget the token while it runs: the page is not connected at once, and does not panic.
        let (text, _) = drawn(&ctx, &mut state, &Conn { token: TokenStatus::Missing, ..conn.clone() });
        assert!(has_line(&text, NOT_CONNECTED), "{text}");
        let model = state.model.clone();
        drop(state);
        let deadline = Instant::now() + Duration::from_secs(20);
        while lock(&model).working.contains_key(B) {
            assert!(Instant::now() < deadline, "the job never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(s.lines().iter().filter(|l| l.starts_with("DELETE")).count(), 1, "the delete went through, once");
        let m = lock(&model);
        assert!(m.tours().is_empty() && m.notice().is_none());
    }

    #[test]
    fn an_unfinished_delete_is_written_down_and_can_be_finished_after_a_restart() {
        // The site starts the delete, then stops making progress; later it finishes.
        let s = stub(|req, seen| match req.line.as_str() {
            "GET /api/v1/tours" => reply(200, if seen == 0 { LIST } else { ONLY_A }),
            "DELETE /api/v1/tours/tour_bbbbbbbbbbbbbbbbbbbbbbbb" => {
                if seen < 4 { reply(202, r#"{"status":"in_progress","done":5,"total":10}"#) } else { reply(200, r#"{"deleted":true}"#) }
            }
            _ => reply(500, r#"{"error":"unexpected"}"#),
        });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        let store = state.unfinished();
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let job = {
            let mut m = lock(&state.model);
            m.ask_delete(B);
            m.confirm_delete(B).unwrap()
        };
        state.spawn_job(job);
        // It is written down while the job still runs: before its first request.
        let deadline = Instant::now() + Duration::from_secs(20);
        while store.for_site(&s.base).is_empty() {
            assert!(Instant::now() < deadline, "the delete was never noted");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(lock(&state.model).work(B).is_some(), "noted before the loop gave up");
        let sentence = unfinished_sentence("Testing");
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading() && m.tours().len() == 1 && !m.wants_load(Instant::now()));
        let flat = text.replace('\n', " ");
        assert!(flat.contains(&sentence) && has_line(&text, FINISH_DELETING) && flat.contains("Use Finish deleting on the Tours page"), "{text}");
        // Refresh and Dismiss leave the block.
        let load = lock(&state.model).refresh().unwrap();
        state.spawn_load(load);
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.notice().is_none());
        assert!(text.replace('\n', " ").contains(&sentence) && has_line(&text, FINISH_DELETING), "{text}");
        // With another site configured it is not offered, and it stays stored.
        let elsewhere = Conn { server: "https://other.example.com".into(), bound: Some("https://other.example.com".into()), ..conn.clone() };
        lock(&state.model).set_site(elsewhere.site());
        let shown = lock(&state.model).shows_unfinished(&store.for_site(&s.base)[0]);
        assert!(!shown);
        assert_eq!(store.for_site(&s.base).len(), 1);

        // The app writes the change to the settings; the next start reads it back.
        let mut settings = crate::settings::Settings::default();
        settings.unfinished_deletes = store.take_changed().expect("a change to write");
        let read: crate::settings::Settings = toml::from_str(&toml::to_string_pretty(&settings).unwrap()).unwrap();
        drop(state);
        let mut state = page();
        let store = state.unfinished();
        store.load(read.unfinished_deletes);
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 1);
        assert!(text.replace('\n', " ").contains(&sentence) && has_line(&text, FINISH_DELETING), "after a restart:\n{text}");
        // "Finish deleting": the same tour is deleted again, and the entry goes when the site is done.
        let entry = store.for_site(&s.base).remove(0);
        let job = lock(&state.model).finish_delete(&entry).unwrap();
        state.spawn_job(job);
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading());
        assert!(!text.contains("did not finish") && !has_line(&text, FINISH_DELETING), "{text}");
        assert!(store.for_site(&s.base).is_empty());
        assert_eq!(store.take_changed(), Some(vec![]), "the settings file is told to forget it");
        assert!(s.lines().iter().filter(|l| l.starts_with("DELETE")).all(|l| l.ends_with(B)));
    }

    #[test]
    fn an_unfinished_delete_whose_tour_is_gone_by_now_is_simply_forgotten() {
        let s = stub(|req, _| if req.line == "GET /api/v1/tours" { reply(200, ONLY_A) } else { reply(404, NO_TOUR) });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        let store = state.unfinished();
        store.load(vec![
            UnfinishedDelete { server: s.base.clone(), tour_id: B.into(), title: "Testing".into() },
            UnfinishedDelete { server: "https://other.example.com".into(), tour_id: C.into(), title: "Elsewhere".into() },
        ]);
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 1);
        let flat = text.replace('\n', " ");
        assert!(flat.contains(&unfinished_sentence("Testing")) && !flat.contains("Elsewhere"), "only this site's entry shows:\n{text}");
        assert_eq!(text.lines().filter(|l| *l == FINISH_DELETING).count(), 1);
        let entry = store.for_site(&s.base).remove(0);
        let job = lock(&state.model).finish_delete(&entry).unwrap();
        state.spawn_job(job);
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading());
        assert!(!text.contains("did not finish"), "{text}");
        // The other site's entry is still stored.
        assert_eq!(store.take_changed(), Some(vec![UnfinishedDelete { server: "https://other.example.com".into(), tour_id: C.into(), title: "Elsewhere".into() }]));
    }

    const DELETE_B: &str = "DELETE /api/v1/tours/tour_bbbbbbbbbbbbbbbbbbbbbbbb";
    const DELETED: &str = r#"{"deleted":true}"#;

    /// The settings file, as far as the notes go: what a job's write-at-once put there.
    type File = Arc<Mutex<Vec<UnfinishedDelete>>>;

    fn written_to(store: &Unfinished) -> File {
        let file = File::default();
        let disk = file.clone();
        store.save_with(move |list| {
            *disk.lock().unwrap() = list.to_vec();
            true
        });
        file
    }

    /// A site stub for one delete of B: `first` answers the first DELETE, `later` the ones after
    /// it (by how many came before). The list has B until `gone_after` list requests were answered.
    /// Also returned: what the settings file held each time a DELETE arrived.
    fn delete_stub(file: &File, gone_after: usize, first: fn() -> Answer, later: fn(usize) -> Answer) -> (Stub, Arc<Mutex<Vec<Vec<UnfinishedDelete>>>>) {
        let at_request: Arc<Mutex<Vec<Vec<UnfinishedDelete>>>> = Arc::default();
        let (file, seen) = (file.clone(), at_request.clone());
        let s = stub(move |req, n| match req.line.as_str() {
            "GET /api/v1/tours" => reply(200, if n < gone_after { LIST } else { ONLY_A }),
            DELETE_B => {
                seen.lock().unwrap().push(file.lock().unwrap().clone());
                if n == 0 { first() } else { later(n) }
            }
            _ => reply(500, r#"{"error":"unexpected"}"#),
        });
        (s, at_request)
    }

    /// Press "Delete tour" on B's confirmation and wait for the job, and the list read after it, to end.
    fn delete_b(ctx: &egui::Context, state: &mut ToursState, conn: &Conn) -> String {
        let job = {
            let mut m = lock(&state.model);
            m.ask_delete(B);
            m.confirm_delete(B).unwrap()
        };
        state.spawn_job(job);
        until(ctx, state, conn, |m, _| m.work(B).is_none() && !m.is_loading() && !m.wants_load(Instant::now()))
    }

    #[test]
    fn a_delete_whose_first_answer_is_lost_is_on_disk_before_the_request_and_is_finished_after_a_restart() {
        // The site removes the tour, but its answer never arrives. Its files are still being cleared.
        let (ctx, mut state) = (crate::theme::test_context(), page());
        let store = state.unfinished();
        let file = written_to(&store);
        let (s, at_request) = delete_stub(&file, 1, || Answer::Drop, |n| if n < 3 { reply(202, r#"{"status":"in_progress","done":5,"total":10}"#) } else { reply(200, DELETED) });
        let conn = stub_conn(&s);
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        assert!(file.lock().unwrap().is_empty());
        let text = delete_b(&ctx, &mut state, &conn);
        let note = entry(&s.base, B, "Testing");
        // The note was in the settings file when the request arrived, not a frame later.
        assert_eq!(*at_request.lock().unwrap(), [vec![note.clone()]]);
        assert_eq!(s.lines().iter().filter(|l| *l == DELETE_B).count(), 1, "a lost answer is not sent again by itself");
        // Nothing said the delete began, yet the note stays: the tour is out of the list, and its block is there.
        assert_eq!(store.for_site(&s.base), [note.clone()]);
        assert_eq!(lock(&state.model).tours().len(), 1);
        let sentence = unfinished_sentence("Testing");
        assert!(text.replace('\n', " ").contains(&sentence) && has_line(&text, FINISH_DELETING), "{text}");

        // The app is closed and started again: the note comes back from the file.
        let mut settings = crate::settings::Settings::default();
        settings.unfinished_deletes = file.lock().unwrap().clone();
        let read: crate::settings::Settings = toml::from_str(&toml::to_string_pretty(&settings).unwrap()).unwrap();
        drop(state);
        let mut state = page();
        let store = state.unfinished();
        store.load(read.unfinished_deletes);
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 1);
        assert!(text.replace('\n', " ").contains(&sentence) && has_line(&text, FINISH_DELETING), "after a restart:\n{text}");
        let job = lock(&state.model).finish_delete(&note).unwrap();
        state.spawn_job(job);
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading());
        assert!(!text.contains("did not finish") && !has_line(&text, FINISH_DELETING), "{text}");
        assert!(store.for_site(&s.base).is_empty());
        assert_eq!(store.take_changed(), Some(vec![]), "the settings file is told to forget it");
    }

    #[test]
    fn a_clear_refusal_before_the_site_began_takes_the_note_away_again() {
        let refusals: [fn() -> Answer; 4] = [
            || reply(409, r#"{"error":"publish_busy","detail":"a publish of this tour is in progress; wait"}"#),
            || reply(401, r#"{"error":"unauthorized"}"#),
            || reply(404, r#"{"error":"not_found"}"#),
            || reply(405, ""),
        ];
        for (i, refusal) in refusals.into_iter().enumerate() {
            let (ctx, mut state) = (crate::theme::test_context(), page());
            let store = state.unfinished();
            let file = written_to(&store);
            let (s, at_request) = delete_stub(&file, usize::MAX, refusal, |_| reply(500, r#"{"error":"unexpected"}"#));
            let conn = stub_conn(&s);
            until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
            let text = delete_b(&ctx, &mut state, &conn);
            // It was noted before the request all the same.
            assert_eq!(*at_request.lock().unwrap(), [vec![entry(&s.base, B, "Testing")]], "refusal {i}");
            // The tour is as it was, the user is told as before, and nothing is left to finish.
            let m = lock(&state.model);
            assert!(m.tours().len() == 2 && m.error(B).is_some_and(|e| e.starts_with("Could not delete the tour: ") || e == "The server rejected the API token."), "refusal {i}: {:?}", m.error(B));
            drop(m);
            assert!(!text.contains("did not finish") && !has_line(&text, FINISH_DELETING), "refusal {i}:\n{text}");
            assert!(store.for_site(&s.base).is_empty(), "refusal {i}");
            assert_eq!(store.take_changed(), Some(vec![]), "refusal {i}: the settings file is told to forget it");
        }
    }

    #[test]
    fn a_delete_that_succeeds_at_once_was_noted_first_and_leaves_no_note() {
        let (ctx, mut state) = (crate::theme::test_context(), page());
        let store = state.unfinished();
        let file = written_to(&store);
        let (s, at_request) = delete_stub(&file, 1, || reply(200, DELETED), |_| reply(404, NO_TOUR));
        let conn = stub_conn(&s);
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let text = delete_b(&ctx, &mut state, &conn);
        assert_eq!(*at_request.lock().unwrap(), [vec![entry(&s.base, B, "Testing")]]);
        assert!(lock(&state.model).tours().len() == 1 && !text.contains("did not finish"), "{text}");
        assert!(store.for_site(&s.base).is_empty());
        assert_eq!(store.take_changed(), Some(vec![]));
    }

    #[test]
    fn an_answer_that_says_nothing_either_way_keeps_the_note_and_deleting_again_clears_it() {
        let unclear: [fn() -> Answer; 5] = [
            || Answer::Drop,
            || reply(500, r#"{"error":"internal"}"#),
            || reply(503, ""),
            || reply(429, r#"{"error":"rate_limited"}"#),
            || reply(200, "<html>Sign in</html>"),
        ];
        for (i, answer) in unclear.into_iter().enumerate() {
            // The site in fact never began: the tour is still there, whole.
            let (ctx, mut state) = (crate::theme::test_context(), page());
            let store = state.unfinished();
            let file = written_to(&store);
            let (s, _) = delete_stub(&file, 2, answer, |_| reply(200, DELETED));
            let conn = stub_conn(&s);
            until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
            let text = delete_b(&ctx, &mut state, &conn);
            let note = entry(&s.base, B, "Testing");
            assert_eq!(store.for_site(&s.base), [note.clone()], "answer {i}");
            assert_eq!(*file.lock().unwrap(), [note.clone()], "answer {i}");
            // While the tour is in the list its row speaks for it: the error, and no block claiming files are left.
            let mut m = lock(&state.model);
            assert!(m.tours().len() == 2 && m.error(B).is_some() && !m.shows_unfinished(&note), "answer {i}");
            assert_eq!(m.finish_delete(&note), None, "answer {i}: a tour in the list is deleted from its row");
            drop(m);
            assert!(!text.contains("did not finish") && !has_line(&text, FINISH_DELETING), "answer {i}:\n{text}");
            // Deleting it again simply deletes it, and the note goes with it.
            delete_b(&ctx, &mut state, &conn);
            assert!(lock(&state.model).tours().len() == 1 && store.for_site(&s.base).is_empty(), "answer {i}");
            assert_eq!(s.lines().iter().filter(|l| *l == DELETE_B).count(), 2, "answer {i}");
        }
    }

    #[test]
    fn a_delete_whose_note_cannot_be_saved_is_not_sent_and_the_row_says_why() {
        // The settings file cannot be written (false), or is not to be written this session (no way to write).
        for can_try in [true, false] {
            let (ctx, mut state) = (crate::theme::test_context(), unsaved_page());
            let store = state.unfinished();
            if can_try {
                store.save_with(|_| false);
            }
            let s = stub(|req, _| if req.line == "GET /api/v1/tours" { reply(200, LIST) } else { reply(200, DELETED) });
            let conn = stub_conn(&s);
            until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
            let text = delete_b(&ctx, &mut state, &conn);
            assert!(s.lines().iter().all(|l| l == "GET /api/v1/tours"), "can_try {can_try}: nothing but list requests: {:?}", s.lines());
            assert!(store.for_site(&s.base).is_empty() && !store.is_changed(), "can_try {can_try}: no note is left in memory");
            let m = lock(&state.model);
            assert_eq!((m.tours().len(), m.error(B), m.notice()), (2, Some(NOTE_NOT_SAVED), None), "can_try {can_try}");
            drop(m);
            assert!(text.replace('\n', " ").contains("Could not save a note about this delete, so nothing was deleted. Check that the settings file can be written, then try again."), "can_try {can_try}:\n{text}");
            assert!(!text.contains("did not finish") && !has_line(&text, FINISH_DELETING), "can_try {can_try}:\n{text}");
            // The file can be written again: the same delete goes through, noted first.
            let file = written_to(&store);
            delete_b(&ctx, &mut state, &conn);
            assert_eq!(s.lines().iter().filter(|l| *l == DELETE_B).count(), 1, "can_try {can_try}");
            assert!(lock(&state.model).error(B).is_none() && store.for_site(&s.base).is_empty(), "can_try {can_try}");
            assert_eq!(*file.lock().unwrap(), [entry(&s.base, B, "Testing")], "can_try {can_try}: it was written before the request");
        }
    }

    #[test]
    fn finish_deleting_needs_no_new_note_so_a_file_that_cannot_be_written_does_not_stop_it() {
        for can_try in [true, false] {
            let (ctx, mut state) = (crate::theme::test_context(), unsaved_page());
            let store = state.unfinished();
            if can_try {
                store.save_with(|_| false);
            }
            let s = stub(|req, _| match req.line.as_str() {
                "GET /api/v1/tours" => reply(200, ONLY_A),
                DELETE_B => reply(200, DELETED),
                _ => reply(500, r#"{"error":"unexpected"}"#),
            });
            let conn = stub_conn(&s);
            let note = entry(&s.base, B, "Testing");
            store.load(vec![note.clone()]);
            until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 1);
            let job = lock(&state.model).finish_delete(&note).unwrap();
            state.spawn_job(job);
            let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading());
            assert_eq!(s.lines().iter().filter(|l| *l == DELETE_B).count(), 1, "can_try {can_try}");
            assert!(store.for_site(&s.base).is_empty(), "can_try {can_try}");
            assert!(!text.contains("Could not save a note") && !has_line(&text, FINISH_DELETING), "can_try {can_try}:\n{text}");
        }
    }

    #[test]
    fn a_refusal_of_finish_deleting_keeps_the_earlier_note() {
        let (ctx, mut state) = (crate::theme::test_context(), page());
        let store = state.unfinished();
        let file = written_to(&store);
        let (s, _) = delete_stub(&file, 0, || reply(401, r#"{"error":"unauthorized"}"#), |_| reply(409, r#"{"error":"publish_busy"}"#));
        let conn = stub_conn(&s);
        let note = entry(&s.base, B, "Testing");
        store.load(vec![note.clone()]);
        until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 1);
        for _ in 0..2 {
            let job = lock(&state.model).finish_delete(&note).unwrap();
            state.spawn_job(job);
            let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading() && m.notice().is_some());
            // The files of the earlier delete are still there: the note is not this job's to take away.
            assert_eq!(store.for_site(&s.base), [note.clone()]);
            assert!(text.replace('\n', " ").contains(&unfinished_sentence("Testing")) && has_line(&text, FINISH_DELETING), "{text}");
        }
        assert!(!store.is_changed() && file.lock().unwrap().is_empty(), "nothing new to write");
    }

    #[test]
    fn a_job_that_panics_releases_its_tour_and_says_so() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let s = stub(|_, _| reply(200, LIST));
        let broken = Arc::new(AtomicBool::new(false));
        let flag = broken.clone();
        let mut state = ToursState::with_token(move |_| {
            assert!(!flag.load(Ordering::SeqCst), "a job thread panics (this message is part of the test)");
            Ok(TOKEN.to_string())
        });
        let (ctx, conn) = (crate::theme::test_context(), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        broken.store(true, Ordering::SeqCst);
        let job = lock(&state.model).start_publish(B).unwrap();
        state.spawn_job(job);
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && m.error(B).is_some());
        assert_eq!(lock(&state.model).error(B), Some(JOB_BROKE));
        assert!(has_line(&text, "Publish") || text.contains("Could not list"), "the row has its buttons back, or the page says the list failed:\n{text}");
        // A list request that panics ends as a failure with Retry, not as an endless spinner.
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && !m.wants_load(Instant::now()));
        assert!(text.replace('\n', " ").contains(LOAD_BROKE) && has_line(&text, "Retry"), "{text}");
        // And the page works again afterwards.
        broken.store(false, Ordering::SeqCst);
        let load = lock(&state.model).refresh().unwrap();
        state.spawn_load(load);
        until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
    }

    #[test]
    fn a_page_whose_token_belongs_to_another_address_sends_nothing() {
        let s = stub(|_, _| reply(200, LIST));
        let (ctx, mut state) = (crate::theme::test_context(), page());
        let conn = Conn { bound: Some("https://tours.example.com".into()), ..stub_conn(&s) };
        let text = drawn(&ctx, &mut state, &conn).0.replace('\n', " ");
        assert!(text.contains("The saved token belongs to tours.example.com, so it is not sent to this address.") && text.contains(OPEN_WORKSPACE), "{text}");
        std::thread::sleep(Duration::from_millis(50));
        drawn(&ctx, &mut state, &conn);
        assert!(s.lines().is_empty(), "the other address got a request: {:?}", s.lines());
    }

    #[test]
    fn the_header_buttons_ask_for_the_right_page() {
        // "Record a tour" is the right-most thing in the header.
        let ctx = crate::theme::test_context();
        let mut state = page();
        let c = conn(TokenStatus::Missing);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(590.0, 520.0));
        let click_at = |ctx: &egui::Context, state: &mut ToursState, at: egui::Pos2| {
            let click = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut went = None;
            for events in [vec![], vec![egui::Event::PointerMoved(at)], vec![click(true)], vec![click(false)], vec![]] {
                let mut page_out = PageOut::default();
                let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| super::ui(ui, state, &c, &mut page_out));
                out.textures_delta.clear();
                went = went.or(page_out.go_to);
            }
            went
        };
        assert_eq!(click_at(&ctx, &mut state, egui::pos2(screen.right() - 40.0, 18.0)), Some(Page::Home), "Record a tour switches to Home and starts nothing");
        // "Open Workspace" sits under the not-connected sentence.
        assert_eq!(click_at(&ctx, &mut state, egui::pos2(50.0, 78.0)), Some(Page::Workspace));
    }

    // ---- Edit, the saved line, and what a delete does to a saved recording ----

    const CH1: &str = "ch_aaaaaaaaaaaaaaaaaaaaaaaa";
    const CH2: &str = "ch_bbbbbbbbbbbbbbbbbbbbbbbb";

    fn key(server: &str, tour: &str, chapter: &str) -> ProjectKey {
        ProjectKey { server: server.into(), tour_id: tour.into(), chapter_id: chapter.into() }
    }

    /// What the store's list says of a saved chapter of a tour on the test site, with no disk behind it.
    fn info(tour: &str, chapter: &str, title: &str, bytes: u64, problem: Option<Problem>) -> ProjectInfo {
        ProjectInfo { key: key(&site().server, tour, chapter), title: title.into(), tour_title: "Customer onboarding".into(), saved_at: 1_763_424_000_000, bytes, frames_bytes: bytes, dir: std::path::PathBuf::from(chapter), problem, form: project::Form::Frames, convert: project::ConvertState::NotTried }
    }

    /// A model with a loaded list and these saved recordings.
    fn loaded_with(tours: Vec<RemoteTour>, saved: Vec<ProjectInfo>) -> Model {
        let mut m = loaded(tours);
        m.apply_saved(&site(), 1, saved, None);
        m
    }

    /// A store of saved recordings in a temporary folder.
    pub(crate) fn temp_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("projects"));
        (tmp, store)
    }

    /// Save a small real recording as the chapter `chapter` of `tour` on the site at `server`, as a publish does.
    pub(crate) fn save_project(store: &Store, server: &str, tour: &str, chapter: &str, title: &str) -> ProjectInfo {
        let recordings = store.root().parent().unwrap().join("recordings");
        std::fs::create_dir_all(&recordings).unwrap();
        let dir = tempfile::tempdir_in(&recordings).unwrap().keep();
        let rec = mythicshot_recording::Recording::synthetic_in(&dir, 64, 48, &[0, 500, 1000], 1500).unwrap();
        let edit = crate::video::Edit::new(&rec, Default::default());
        let snap = project::Snapshot { rec: &rec, title, tour_title: "Customer onboarding", steps: 2, edit: &edit, events: &[], items: &[] };
        store.save(&key(server, tour, chapter), &snap).unwrap().info
    }

    /// Every file under `dir`, by its path from there, with its bytes.
    fn files(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        fn walk(dir: &std::path::Path, from: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if e.file_type().unwrap().is_dir() {
                    walk(&e.path(), from, out);
                } else {
                    out.push((e.path().strip_prefix(from).unwrap().to_string_lossy().replace('\\', "/"), std::fs::read(e.path()).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    /// The two tours as the site lists them: A with two chapters, B with one.
    fn listed() -> Vec<RemoteTour> {
        let mut list = two();
        list[1].chapters = 1;
        list
    }

    fn mb(n: f64) -> u64 {
        (n * 1_048_576.0) as u64
    }

    #[test]
    fn a_tour_with_a_saved_recording_says_so_with_its_size() {
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap();
        let mut t = tour(A, "Customer onboarding", true, 22);
        t.published_at = Some(now.timestamp_millis());
        let one = [info(A, CH1, "Chapter one", mb(12.4), None)];
        assert_eq!(saved_note(&one, None).as_deref(), Some("recording saved on this computer (12.4 MB)"));
        assert_eq!(detail_line(&t, &one, None, &now), "2 chapters · 22 steps · published 18 Nov · recording saved on this computer (12.4 MB)");
        // Under a megabyte the size is in KB.
        let small = [info(A, CH1, "Chapter one", 840 * 1024, None)];
        assert_eq!(detail_line(&t, &small, None, &now), "2 chapters · 22 steps · published 18 Nov · recording saved on this computer (840 KB)");
        // One per saved chapter: counted, with their sizes together.
        let two = [info(A, CH1, "Chapter one", mb(12.4), None), info(A, CH2, "Chapter two", mb(18.6), None)];
        assert_eq!(detail_line(&t, &two, None, &now), "2 chapters · 22 steps · published 18 Nov · 2 recordings saved on this computer (31.0 MB)");
        // After the date when there is one, and after the counts when there is none.
        t.published_at = None;
        assert_eq!(detail_line(&t, &one, None, &now), "2 chapters · 22 steps · recording saved on this computer (12.4 MB)");
        // Nothing saved: the line is as it always was.
        assert_eq!(saved_note(&[], None), None);
        assert_eq!(detail_line(&t, &[], None, &now), "2 chapters · 22 steps");
    }

    #[test]
    fn a_tour_whose_saved_recording_has_a_problem_does_not_claim_one() {
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap();
        let mut t = tour(A, "Customer onboarding", true, 22);
        t.published_at = None;
        for problem in [Problem::Damaged, Problem::Newer, Problem::FramesMissing] {
            let saved = [info(A, CH1, "Chapter one", mb(12.4), Some(problem))];
            assert_eq!(saved_note(&saved, None), None, "{problem:?}");
            assert_eq!(detail_line(&t, &saved, None, &now), "2 chapters · 22 steps", "{problem:?}");
            // Beside one that opens, only that one is counted, with its own size.
            let mixed = [saved[0].clone(), info(A, CH2, "Chapter two", mb(3.0), None)];
            assert_eq!(saved_note(&mixed, None).as_deref(), Some("recording saved on this computer (3.0 MB)"), "{problem:?}");
        }
    }

    #[test]
    fn every_row_has_edit_after_open_or_first_among_the_manage_buttons() {
        use RowButton::*;
        let public = tour(A, "A", true, 3);
        assert_eq!(row_buttons(&public), [CopyLink, Open, Edit, Rename, Unpublish, Delete]);
        let mut unlinked = public.clone();
        unlinked.link = None;
        assert_eq!(row_buttons(&unlinked), [Edit, Rename, Unpublish, Delete]);
        assert_eq!(row_buttons(&tour(B, "B", false, 3)), [Publish, Edit, Rename, Delete]);
        assert_eq!(Edit.label(), "Edit");
    }

    #[test]
    fn edit_without_a_saved_recording_says_it_cannot_be_edited_here() {
        let mut m = loaded(two());
        assert_eq!(m.ask_edit(B), None);
        assert_eq!(m.error(B), Some("This tour's recording is not saved on this computer, so it can't be edited here."));
        assert_eq!(m.edit(), &Edit::None, "nothing opens, not even a question");
        assert_eq!(m.error(A), None, "the other row says nothing");
        // A recording saved for the other tour does not count for this one.
        let mut m = loaded_with(two(), vec![info(A, CH1, "Chapter one", mb(1.0), None)]);
        assert_eq!(m.ask_edit(B), None);
        assert_eq!(m.error(B), Some(NOT_SAVED));
        // A tour that is not in the list, or has a job running, is not asked about at all.
        assert_eq!(m.ask_edit(C), None);
        assert_eq!(m.error(C), None);
        let mut m = loaded_with(listed(), vec![info(B, CH1, "Chapter one", mb(1.0), None)]);
        let job = m.start_publish(B).unwrap();
        assert_eq!(m.ask_edit(B), None);
        assert_eq!((m.error(B), m.edit()), (None, &Edit::None));
        m.finish(&job, Err(Failure::Message("Could not publish the tour.".into())));
        assert!(m.ask_edit(B).is_some(), "and can be edited again once the job is over");
        assert_eq!(m.error(B), None, "Edit takes the row's older message away");
    }

    #[test]
    fn a_saved_recording_with_a_problem_says_which_when_edit_is_pressed() {
        let cases = [
            (Problem::Damaged, "This tour's saved recording is damaged, so it can't be opened here. The tour on the site is not affected."),
            (Problem::Newer, "This tour's recording was saved by a newer version of Mythicshot. Update Mythicshot to edit it."),
            (Problem::FramesMissing, "This tour's saved recording is missing its frames, so it can't be opened here. The tour on the site is not affected."),
        ];
        for (problem, sentence) in cases {
            let mut m = loaded_with(two(), vec![info(B, CH1, "Chapter one", mb(1.0), Some(problem))]);
            assert_eq!(m.ask_edit(B), None, "{problem:?}");
            assert_eq!(m.error(B), Some(sentence), "{problem:?}");
            assert_eq!(m.edit(), &Edit::None, "{problem:?}");
            assert_eq!(problem_sentence(problem), sentence);
            // The same for a tour with chapters: there is nothing to choose from.
            let mut m = loaded_with(two(), vec![info(A, CH1, "Chapter one", mb(1.0), Some(problem))]);
            assert_eq!(m.ask_edit(A), None, "{problem:?}");
            assert_eq!((m.error(A), m.edit()), (Some(sentence), &Edit::None), "{problem:?}");
        }
        // What the store says when the recording does not open after all.
        assert_eq!(load_sentence(&LoadError::NotSaved), Some(NOT_SAVED));
        assert_eq!(load_sentence(&LoadError::Damaged("the frames file is not the one the project was saved with".into())), Some(SAVED_DAMAGED));
        assert_eq!(load_sentence(&LoadError::Newer), Some(SAVED_NEWER));
        assert_eq!(load_sentence(&LoadError::AlreadyOpen), Some("This tour's recording is already open in an editor."));
        for s in [NOT_SAVED, SAVED_DAMAGED, SAVED_NEWER, SAVED_FRAMES_MISSING, ALREADY_OPEN, SAVED_NOT_REMOVED] {
            assert!(s.ends_with('.') && !s.contains('!'), "{s}");
        }
    }

    #[test]
    fn edit_of_a_one_chapter_tour_asks_for_its_project() {
        // B has one chapter.
        let mut m = loaded_with(listed(), vec![info(B, CH1, "Chapter one", mb(1.0), None), info(A, CH2, "Elsewhere", mb(1.0), None)]);
        assert_eq!(m.ask_edit(B), Some(key(&site().server, B, CH1)));
        assert_eq!((m.edit(), m.error(B)), (&Edit::None, None), "no question and no message");
        // Pressed again: the same answer; whether it is open already is the store's to say.
        assert_eq!(m.ask_edit(B), Some(key(&site().server, B, CH1)));
        // It replaces whatever else was open on that row.
        m.ask_delete(B);
        assert!(m.ask_edit(B).is_some());
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!(m.confirm_delete(B), None, "the confirmation is gone with it");
    }

    #[test]
    fn a_tour_with_several_chapters_asks_which_and_offers_only_saved_ones() {
        // A has two chapters; only one is saved. It asks all the same.
        let mut m = loaded_with(two(), vec![info(A, CH1, "Chapter one", mb(1.0), None), info(B, CH2, "Other tour", mb(1.0), None)]);
        assert_eq!(m.ask_edit(A), None);
        assert_eq!(m.edit(), &Edit::ChoosingChapter { id: A.into() });
        assert_eq!(usable(m.saved(A)).map(chapter_label).collect::<Vec<_>>(), ["Chapter one"]);
        // A chapter that is not saved, or belongs to another tour, is not an answer.
        assert_eq!(m.choose_chapter(CH2), None);
        assert_eq!(m.edit(), &Edit::ChoosingChapter { id: A.into() }, "the question stays");
        assert_eq!(m.choose_chapter(CH1), Some(key(&site().server, A, CH1)));
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!(m.choose_chapter(CH1), None, "a second click finds no question to answer");
        // Cancel.
        m.ask_edit(A);
        m.cancel_edit();
        assert_eq!((m.choose_chapter(CH1), m.edit()), (None, &Edit::None));
        // Both saved, one of them damaged: only the one that opens is offered.
        let mut m = loaded_with(two(), vec![info(A, CH1, "Chapter one", mb(1.0), Some(Problem::Damaged)), info(A, CH2, "", mb(1.0), None)]);
        assert_eq!(m.ask_edit(A), None);
        assert_eq!(usable(m.saved(A)).map(chapter_label).collect::<Vec<_>>(), ["Untitled chapter"]);
        assert_eq!(m.choose_chapter(CH1), None, "the damaged one cannot be chosen");
        assert_eq!(m.choose_chapter(CH2), Some(key(&site().server, A, CH2)));
        // Two saved chapters of a tour the site counts one chapter for: it still asks.
        let mut m = loaded_with(listed(), vec![info(B, CH1, "One", mb(1.0), None), info(B, CH2, "Two", mb(1.0), None)]);
        assert_eq!(m.ask_edit(B), None);
        assert_eq!(m.edit(), &Edit::ChoosingChapter { id: B.into() });
        // The question is one thing being edited: no timer reads the list away under it.
        assert!(!m.wants_load(Instant::now() + STALE_AFTER + Duration::from_secs(1)));
        // A job on that tour closes it.
        let job = m.start_publish(B).unwrap();
        assert_eq!((m.choose_chapter(CH1), m.edit()), (None, &Edit::None));
        m.finish(&job, Ok(Done::Published(None)));
        // The saved chapters go while the question is up (the tour was deleted elsewhere in the app): it closes.
        m.ask_edit(B);
        assert_eq!(m.edit(), &Edit::ChoosingChapter { id: B.into() });
        m.apply_saved(&site(), 2, vec![], None);
        assert_eq!((m.choose_chapter(CH1), m.edit()), (None, &Edit::None));
    }

    #[test]
    fn a_refresh_that_removes_the_tour_closes_the_chapter_question() {
        let saved = || vec![info(A, CH1, "Chapter one", mb(1.0), None), info(A, CH2, "Chapter two", mb(1.0), None)];
        let mut m = loaded_with(two(), saved());
        m.ask_edit(A);
        // The same tours in another order: it is addressed by id, so the question stays.
        reload(&mut m, vec![two()[1].clone(), two()[0].clone()]);
        assert_eq!(m.edit(), &Edit::ChoosingChapter { id: A.into() });
        // The tour is gone.
        reload(&mut m, vec![two()[1].clone()]);
        assert_eq!(m.edit(), &Edit::None);
        assert_eq!(m.notice().map(|n| n.text.as_str()), Some(publish::TOUR_GONE));
        assert_eq!(m.choose_chapter(CH1), None, "nothing is opened for a tour that is not in the list");
        // A list that cannot be read closes it too.
        let mut m = loaded_with(two(), saved());
        m.ask_edit(A);
        let (generation, _) = m.begin_load().unwrap();
        m.finish_load(generation, Err("Could not list the tours: the server did not answer in time.".into()), Instant::now());
        assert_eq!((m.choose_chapter(CH1), m.edit()), (None, &Edit::None));
    }

    #[test]
    fn the_delete_question_adds_the_saved_recording_sentence_only_when_there_is_one() {
        let t = tour(A, "Customer onboarding", true, 14);
        assert_eq!(delete_question(&t, 1), "Delete \"Customer onboarding\"? Its 14 steps and images are removed from the site. The recording saved on this computer is deleted too. This can't be undone.");
        assert_eq!(delete_question(&t, 0), "Delete \"Customer onboarding\"? Its 14 steps and images are removed from the site. This can't be undone.");
        // Several saved chapters go with the tour, and the sentence says so.
        for several in [2, 3, 40] {
            assert_eq!(delete_question(&t, several), "Delete \"Customer onboarding\"? Its 14 steps and images are removed from the site. The recordings saved on this computer are deleted too. This can't be undone.");
        }
    }

    #[test]
    fn a_project_saved_for_another_address_is_not_shown() {
        // The address was changed in Workspace: the page belongs to the new one.
        let (_tmp, store) = temp_store();
        let old = "https://old.example.com";
        save_project(&store, old, A, CH1, "Chapter one");
        let state = unsaved_page().with_store(store.clone());
        let (_, saved, _) = read_saved(&state.tools, &site().server);
        assert!(saved.is_empty(), "{saved:?}");
        let mut m = loaded(two());
        m.apply_saved(&site(), 1, saved, None);
        assert!(m.saved(A).is_empty());
        assert_eq!(m.ask_edit(A), None);
        assert_eq!(m.error(A), Some(NOT_SAVED));
        // Even handed to the model by mistake, a recording of another address is not kept.
        let (_, theirs, _) = read_saved(&state.tools, old);
        assert_eq!(theirs.len(), 1);
        m.apply_saved(&site(), 2, theirs.clone(), None);
        assert!(m.saved(A).is_empty());
        // With the old address set again it is back; a read for the address before is dropped.
        let old_site = Site { server: old.into(), epoch: 1 };
        m.set_site(Some(old_site.clone()));
        assert!(m.saved(A).is_empty(), "what was read for the other address went with the list");
        let (generation, _) = m.begin_load().unwrap();
        m.finish_load(generation, Ok(two()), Instant::now());
        m.apply_saved(&site(), 3, vec![info(A, CH2, "Wrong site", 1, None)], None);
        assert!(m.saved(A).is_empty());
        m.apply_saved(&old_site, 3, theirs.clone(), None);
        assert_eq!(m.saved(A), theirs.as_slice());
        // The recording itself was never touched.
        assert_eq!(store.for_tour(old, A).len(), 1);
    }

    #[test]
    fn the_saved_recordings_are_read_again_only_when_the_store_changed() {
        let mut m = Model::default();
        assert!(!m.wants_saved(7) && m.begin_saved().is_none(), "no site, nothing to read");
        m.set_site(Some(site()));
        assert!(!m.wants_saved(7), "the first list request reads them");
        let (generation, _) = m.begin_load().unwrap();
        assert!(!m.wants_saved(7));
        m.apply_saved(&site(), 7, vec![info(A, CH1, "Chapter one", mb(1.0), None)], None);
        m.finish_load(generation, Ok(two()), Instant::now());
        assert!(!m.wants_saved(7), "nothing changed since");
        // Something was saved or removed.
        assert!(m.wants_saved(8));
        assert_eq!(m.begin_saved(), Some(site()));
        assert!(!m.wants_saved(8) && m.begin_saved().is_none(), "one read at a time");
        m.finish_saved(&site(), 8, vec![], None);
        assert!(m.saved(A).is_empty());
        assert!(!m.wants_saved(8) && m.wants_saved(9));
        // An older read that arrives late does not bring back what a later one no longer found.
        m.apply_saved(&site(), 7, vec![info(A, CH1, "Chapter one", mb(1.0), None)], None);
        assert!(m.saved(A).is_empty());
        // A list that failed shows no rows: nothing is read for it until it is read again.
        let (generation, _) = m.begin_load().unwrap();
        m.finish_load(generation, Err("Could not list the tours.".into()), Instant::now());
        assert!(!m.wants_saved(9));
    }

    #[test]
    fn a_recording_that_does_not_open_after_all_says_why_on_its_row() {
        let mut m = loaded(two());
        m.edit_failed(&key(&site().server, B, CH1), ALREADY_OPEN);
        assert_eq!(m.error(B), Some(ALREADY_OPEN));
        assert_eq!(m.notice(), None);
        // Its row is gone by now: the sentence goes above the list.
        m.edit_failed(&key(&site().server, C, CH1), SAVED_DAMAGED);
        assert_eq!(m.notice(), Some(&Notice { text: SAVED_DAMAGED.into(), error: true }));
        // The page belongs to another address by now: nothing is said there.
        let mut m = loaded(two());
        m.edit_failed(&key("https://other.example.com", B, CH1), SAVED_DAMAGED);
        assert_eq!((m.error(B), m.notice()), (None, None));
    }

    /// The centre of every piece of text equal to `text` that one frame of the page draws.
    fn places(ctx: &egui::Context, state: &mut ToursState, conn: &Conn, text: &str) -> Vec<egui::Pos2> {
        fn collect(shape: &egui::Shape, text: &str, into: &mut Vec<egui::Pos2>) {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == text => into.push(t.pos + t.galley.size() / 2.0),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, text, into)),
                _ => {}
            }
        }
        let mut page_out = PageOut::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(590.0, 520.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| super::ui(ui, state, conn, &mut page_out));
        out.textures_delta.clear();
        let mut found = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, text, &mut found));
        found
    }

    /// Click at `at` in the page (into the test's own egui context; no real input is sent) and
    /// say which saved recording the page asked the app to open.
    fn click(ctx: &egui::Context, state: &mut ToursState, conn: &Conn, at: egui::Pos2) -> Option<ProjectKey> {
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut asked = None;
        for events in [vec![egui::Event::PointerMoved(at)], vec![press(true)], vec![press(false)], vec![]] {
            let mut page_out = PageOut::default();
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(590.0, 520.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| super::ui(ui, state, conn, &mut page_out));
            out.textures_delta.clear();
            assert!(asked.is_none() || page_out.edit_project.is_none(), "one click asks once");
            asked = asked.or(page_out.edit_project);
        }
        asked
    }

    #[test]
    fn the_page_draws_the_saved_line_and_the_edit_button() {
        let s = stub(|req, _| if req.line == "GET /api/v1/tours" { reply(200, LIST) } else { reply(500, r#"{"error":"unexpected"}"#) });
        let (_tmp, store) = temp_store();
        // A (two chapters) has one chapter saved, B (one chapter) its only one.
        save_project(&store, &s.base, A, CH1, "Getting started");
        let saved_b = save_project(&store, &s.base, B, CH1, "Testing");
        let (ctx, mut state, conn) = (crate::theme::test_context(), page().with_store(store.clone()), stub_conn(&s));
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
        assert_eq!(text.lines().filter(|l| *l == "Edit").count(), 2, "every row has Edit:\n{text}");
        let flat = text.replace('\n', " ");
        assert!(flat.contains("2 chapters · 22 steps · published ") && flat.contains("1 chapter · 9 steps · updated "), "{text}");
        assert_eq!(flat.matches(" · recording saved on this computer (").count(), 2, "{text}");
        assert!(flat.contains(&format!("updated {} · recording saved on this computer ({})", day_label(1_763_424_000_000, &Local::now()).unwrap(), publish_ui::size_text(saved_b.bytes))), "{text}");
        assert!(!flat.contains(WHICH_CHAPTER) && !flat.contains("can't be edited"), "{text}");
        // Edit on B, the tour with one chapter: the app is asked to open exactly that recording.
        let edits = places(&ctx, &mut state, &conn, "Edit");
        assert_eq!(edits.len(), 2);
        assert_eq!(click(&ctx, &mut state, &conn, edits[1]), Some(saved_b.key.clone()));
        // Edit on A, which has two chapters: the question, with the saved chapter only.
        assert_eq!(click(&ctx, &mut state, &conn, edits[0]), None);
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert!(has_line(&text, WHICH_CHAPTER) && has_line(&text, "Getting started") && has_line(&text, "Cancel"), "{text}");
        assert_eq!(text.lines().filter(|l| *l == "Edit").count(), 1, "the row that asks shows no other buttons:\n{text}");
        assert!(!has_line(&text, "Copy link") && has_line(&text, "Publish"), "{text}");
        let chapter = places(&ctx, &mut state, &conn, "Getting started");
        assert_eq!(chapter.len(), 1);
        assert_eq!(click(&ctx, &mut state, &conn, chapter[0]), Some(key(&s.base, A, CH1)));
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert!(!has_line(&text, WHICH_CHAPTER) && has_line(&text, "Copy link"), "the question is answered:\n{text}");
        // Cancel closes the question and asks for nothing.
        lock(&state.model).ask_edit(A);
        let cancel = places(&ctx, &mut state, &conn, "Cancel");
        assert_eq!(click(&ctx, &mut state, &conn, cancel[0]), None);
        assert_eq!(lock(&state.model).edit(), &Edit::None);
        // The delete confirmation of a tour with a saved recording says the recording goes too.
        lock(&state.model).ask_delete(B);
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert!(text.replace('\n', " ").contains("Delete \"Testing\"? Its 9 steps and images are removed from the site. The recording saved on this computer is deleted too. This can't be undone."), "{text}");
        // Nothing but the list was asked of the site, and looking never changed a saved recording.
        assert!(s.lines().iter().all(|l| l == "GET /api/v1/tours"), "{:?}", s.lines());
        assert_eq!(store.for_site(&s.base).len(), 2);
    }

    #[test]
    fn a_computer_with_no_folder_for_saved_recordings_says_the_recording_is_not_saved() {
        let s = stub(|_, _| reply(200, LIST));
        // `page()` has no folder, as when the user's folder cannot be found.
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
        assert!(!text.contains("saved on this computer"), "{text}");
        let edits = places(&ctx, &mut state, &conn, "Edit");
        assert_eq!(click(&ctx, &mut state, &conn, edits[1]), None);
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert!(text.replace('\n', " ").contains("This tour's recording is not saved on this computer, so it can't be edited here."), "{text}");
        // The confirmation is the one without the saved recording's sentence.
        lock(&state.model).ask_delete(B);
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert!(text.replace('\n', " ").contains("are removed from the site. This can't be undone."), "{text}");
    }

    #[test]
    fn a_damaged_or_newer_project_shows_its_own_sentence_on_the_row() {
        let cases: [(fn(&std::path::Path), &str); 3] = [
            (|dir| std::fs::write(dir.join("project.json"), b"{ not a project").unwrap(), SAVED_DAMAGED),
            (
                |dir| {
                    let text = std::fs::read_to_string(dir.join("project.json")).unwrap();
                    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
                    v["format"] = serde_json::json!(project::FORMAT + 1);
                    std::fs::write(dir.join("project.json"), v.to_string()).unwrap();
                },
                SAVED_NEWER,
            ),
            (|dir| std::fs::remove_file(dir.join("frames.zst")).unwrap(), SAVED_FRAMES_MISSING),
        ];
        for (i, (spoil, sentence)) in cases.into_iter().enumerate() {
            let s = stub(|_, _| reply(200, LIST));
            let (_tmp, store) = temp_store();
            let saved = save_project(&store, &s.base, B, CH1, "Testing");
            spoil(&saved.dir);
            let before = files(&saved.dir);
            let (ctx, mut state, conn) = (crate::theme::test_context(), page().with_store(store.clone()), stub_conn(&s));
            let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
            assert!(!text.contains("saved on this computer"), "case {i}: the row claims no recording:\n{text}");
            assert_eq!(text.lines().filter(|l| *l == "Edit").count(), 2, "case {i}: Edit is there all the same");
            let edits = places(&ctx, &mut state, &conn, "Edit");
            assert_eq!(click(&ctx, &mut state, &conn, edits[1]), None, "case {i}: nothing is opened");
            let (text, _) = drawn(&ctx, &mut state, &conn);
            assert!(text.replace('\n', " ").contains(sentence), "case {i}:\n{text}");
            assert!(!text.contains(NOT_SAVED), "case {i}: it says which case it is:\n{text}");
            // Opening it through the store all the same ends in a sentence, never in an editor.
            let opened = crate::open_saved(Some(&store), &saved.key, &crate::settings::Settings::default());
            assert!(matches!(opened, Err(e) if e == SAVED_DAMAGED || e == sentence), "case {i}");
            // Never a crash and never a delete: the folder is exactly as it was.
            assert!(files(&saved.dir) == before, "case {i}: the folder changed");
            // It is this computer's all the same, and goes with the tour: the question says so.
            lock(&state.model).ask_delete(B);
            let (text, _) = drawn(&ctx, &mut state, &conn);
            assert!(text.replace('\n', " ").contains("The recording saved on this computer is deleted too."), "case {i}:\n{text}");
        }
    }

    #[test]
    fn edit_pressed_for_a_project_that_is_already_open_opens_nothing_new() {
        let (_tmp, store) = temp_store();
        let server = site().server;
        let saved = save_project(&store, &server, B, CH1, "Testing");
        let settings = crate::settings::Settings::default();
        let first = crate::open_saved(Some(&store), &saved.key, &settings).expect("the saved recording opens in an editor");
        // Edit again, while that editor is open.
        let again = crate::open_saved(Some(&store), &saved.key, &settings);
        assert_eq!(again.err(), Some("This tour's recording is already open in an editor."));
        let mut m = loaded_with(listed(), vec![saved.clone()]);
        assert_eq!(m.ask_edit(B), Some(saved.key.clone()));
        m.edit_failed(&saved.key, ALREADY_OPEN);
        assert_eq!(m.error(B), Some(ALREADY_OPEN));
        // The editor is closed: it opens again.
        drop(first);
        let reopened = crate::open_saved(Some(&store), &saved.key, &settings);
        assert!(reopened.is_ok());
        drop(reopened);
        // No folder for saved recordings, or no recording for that chapter: the not-saved sentence.
        assert_eq!(crate::open_saved(None, &saved.key, &settings).err(), Some(NOT_SAVED));
        assert_eq!(crate::open_saved(Some(&store), &key(&server, B, CH2), &settings).err(), Some(NOT_SAVED));
        assert_eq!(crate::open_saved(Some(&store), &key("https://other.example.com", B, CH1), &settings).err(), Some(NOT_SAVED));
        assert_eq!(store.for_tour(&server, B), vec![saved], "opening and failing to open changed nothing");
    }

    /// The saved recordings of a delete test: two chapters of B and one of A on the stub's site,
    /// and one of B on another site.
    fn saved_for_delete(store: &Store, s: &Stub) -> [ProjectInfo; 4] {
        [save_project(store, &s.base, B, CH1, "One"), save_project(store, &s.base, B, CH2, "Two"), save_project(store, &s.base, A, CH1, "Other tour"), save_project(store, "https://other.example.com", B, CH1, "Other site")]
    }

    #[test]
    fn a_confirmed_delete_removes_the_saved_recordings_of_that_tour_only() {
        let (ctx, state) = (crate::theme::test_context(), page());
        let file = written_to(&state.unfinished());
        // The site takes two calls: the saved recordings are still there while it is asked.
        let (s, _) = delete_stub(&file, 1, || reply(202, r#"{"status":"in_progress","done":5,"total":10}"#), |_| reply(200, DELETED));
        let (_tmp, store) = temp_store();
        let [b1, b2, a1, elsewhere] = saved_for_delete(&store, &s);
        let mut state = state.with_store(store.clone());
        let conn = stub_conn(&s);
        let text = until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        assert!(text.contains("2 recordings saved on this computer"), "{text}");
        // Asking, and "Keep it", remove nothing.
        lock(&state.model).ask_delete(B);
        drawn(&ctx, &mut state, &conn);
        lock(&state.model).cancel_edit();
        assert_eq!(store.for_tour(&s.base, B).len(), 2);
        let text = delete_b(&ctx, &mut state, &conn);
        assert_eq!(s.lines().iter().filter(|l| *l == DELETE_B).count(), 2);
        let m = lock(&state.model);
        assert_eq!((m.tours().len(), m.notice()), (1, None), "deleted, with nothing to say about the recording");
        drop(m);
        // Exactly that address's and that tour's recordings are gone.
        assert!(!b1.dir.exists() && !b2.dir.exists(), "{:?}", files(store.root()));
        assert_eq!(store.for_tour(&s.base, B), vec![]);
        assert_eq!(store.for_tour(&s.base, A), vec![a1.clone()]);
        assert_eq!(store.for_tour("https://other.example.com", B), vec![elsewhere.clone()]);
        assert_eq!(store.list().len(), 2);
        assert!(text.contains(" · recording saved on this computer ("), "the other tour's line stays:\n{text}");
    }

    #[test]
    fn a_tour_deleted_while_its_recording_is_open_loses_it_when_the_editor_closes() {
        let (ctx, state) = (crate::theme::test_context(), page());
        let file = written_to(&state.unfinished());
        let (s, _) = delete_stub(&file, 1, || reply(200, DELETED), |_| reply(404, NO_TOUR));
        let (_tmp, store) = temp_store();
        let saved = save_project(&store, &s.base, B, CH1, "Testing");
        let mut state = state.with_store(store.clone());
        let conn = stub_conn(&s);
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let editor = crate::open_saved(Some(&store), &saved.key, &crate::settings::Settings::default()).expect("it opens");
        delete_b(&ctx, &mut state, &conn);
        let m = lock(&state.model);
        assert_eq!((m.tours().len(), m.notice()), (1, None));
        drop(m);
        // The editor still reads its files; the recording is no longer listed or offered.
        assert!(saved.dir.join("frames.zst").is_file());
        assert_eq!(store.list(), vec![]);
        drop(editor);
        assert!(!saved.dir.exists(), "{:?}", files(store.root()));
    }

    #[test]
    fn a_delete_the_site_refused_keeps_the_saved_recording() {
        // A clear refusal, and every answer that says nothing either way: the tour may well be there.
        let answers: [fn() -> Answer; 9] = [
            || reply(409, r#"{"error":"publish_busy","detail":"a publish of this tour is in progress; wait"}"#),
            || reply(401, r#"{"error":"unauthorized"}"#),
            || reply(404, r#"{"error":"not_found"}"#),
            || reply(405, ""),
            || Answer::Drop,
            || reply(500, r#"{"error":"internal"}"#),
            || reply(503, ""),
            || reply(429, r#"{"error":"rate_limited"}"#),
            || reply(200, "<html>Sign in</html>"),
        ];
        for (i, answer) in answers.into_iter().enumerate() {
            let (ctx, state) = (crate::theme::test_context(), page());
            let file = written_to(&state.unfinished());
            let (s, at_request) = delete_stub(&file, usize::MAX, answer, |_| reply(500, r#"{"error":"unexpected"}"#));
            let (_tmp, store) = temp_store();
            let saved = saved_for_delete(&store, &s);
            let before = files(store.root());
            let mut state = state.with_store(store.clone());
            let conn = stub_conn(&s);
            until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
            let text = delete_b(&ctx, &mut state, &conn);
            assert_eq!(at_request.lock().unwrap().len(), 1, "answer {i}: the delete was sent");
            let m = lock(&state.model);
            assert!(m.tours().len() == 2 && m.error(B).is_some(), "answer {i}: the tour is still listed, with the reason");
            assert_eq!(m.saved(B).len(), 2, "answer {i}");
            drop(m);
            // Not a file of any saved recording changed, and no removal was even begun.
            let after = files(store.root());
            assert!(after == before, "answer {i}: a file of a saved recording changed");
            assert!(after.iter().all(|(name, _)| !name.ends_with(".removing")), "answer {i}");
            let mut all = saved.to_vec();
            all.sort_by(|a, b| a.dir.cmp(&b.dir));
            assert_eq!(store.list(), all, "answer {i}");
            assert!(text.contains("2 recordings saved on this computer") && !text.contains(SAVED_NOT_REMOVED), "answer {i}:\n{text}");
            // And it still opens.
            assert!(crate::open_saved(Some(&store), &saved[0].key, &crate::settings::Settings::default()).is_ok(), "answer {i}");
        }
    }

    #[test]
    fn a_delete_that_was_never_sent_keeps_the_saved_recording() {
        // The note of the delete cannot be written, or the token cannot be read: no request goes out.
        let s = stub(|req, _| if req.line == "GET /api/v1/tours" { reply(200, LIST) } else { reply(200, DELETED) });
        let (_tmp, store) = temp_store();
        saved_for_delete(&store, &s);
        let before = files(store.root());
        let (ctx, mut state, conn) = (crate::theme::test_context(), unsaved_page().with_store(store.clone()), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        delete_b(&ctx, &mut state, &conn);
        assert_eq!(lock(&state.model).error(B), Some(NOTE_NOT_SAVED));
        assert!(s.lines().iter().all(|l| l == "GET /api/v1/tours"));
        assert!(files(store.root()) == before, "a file of a saved recording changed");
    }

    #[test]
    fn a_delete_that_left_files_on_the_site_still_removes_the_saved_recording() {
        // The site has removed the tour and stops making progress on its files.
        let (ctx, state) = (crate::theme::test_context(), page());
        let file = written_to(&state.unfinished());
        let (s, _) = delete_stub(&file, 1, || reply(202, r#"{"status":"in_progress","done":5,"total":10}"#), |_| reply(202, r#"{"status":"in_progress","done":5,"total":10}"#));
        let (_tmp, store) = temp_store();
        let [b1, b2, a1, elsewhere] = saved_for_delete(&store, &s);
        let mut state = state.with_store(store.clone());
        let conn = stub_conn(&s);
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let text = delete_b(&ctx, &mut state, &conn);
        let flat = text.replace('\n', " ");
        assert!(flat.contains(&unfinished_sentence("Testing")) && has_line(&text, FINISH_DELETING), "{text}");
        assert!(!flat.contains(SAVED_NOT_REMOVED), "{text}");
        // The tour is gone from the site either way, so its recordings are gone from this computer.
        assert!(!b1.dir.exists() && !b2.dir.exists());
        let mut left = vec![a1, elsewhere];
        left.sort_by(|a, b| a.dir.cmp(&b.dir));
        assert_eq!(store.list(), left);
        // "Finish deleting" later finds nothing more to remove here, and says nothing about it.
        let note = entry(&s.base, B, "Testing");
        let job = lock(&state.model).finish_delete(&note).unwrap();
        state.spawn_job(job);
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading());
        assert!(!text.contains(SAVED_NOT_REMOVED), "{text}");
        assert_eq!(store.list().len(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn a_saved_recording_that_cannot_be_removed_is_a_notice_and_the_tour_is_still_deleted() {
        use std::os::windows::fs::OpenOptionsExt;
        // Once for a delete that finishes, once for one that leaves files on the site.
        for unfinished in [false, true] {
            let (ctx, state) = (crate::theme::test_context(), page());
            let file = written_to(&state.unfinished());
            let in_progress = || reply(202, r#"{"status":"in_progress","done":5,"total":10}"#);
            let (s, _) = if unfinished { delete_stub(&file, 1, in_progress, |_| reply(202, r#"{"status":"in_progress","done":5,"total":10}"#)) } else { delete_stub(&file, 1, || reply(200, DELETED), |_| reply(404, NO_TOUR)) };
            let (_tmp, store) = temp_store();
            let saved = save_project(&store, &s.base, B, CH1, "Testing");
            let mut state = state.with_store(store.clone());
            let conn = stub_conn(&s);
            until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
            // Another program has the frames file open and does not let it be deleted.
            let held = std::fs::OpenOptions::new().read(true).share_mode(1 | 2).open(saved.dir.join("frames.zst")).unwrap();
            let text = delete_b(&ctx, &mut state, &conn);
            let m = lock(&state.model);
            assert_eq!(m.tours().len(), 1, "unfinished {unfinished}: the tour is deleted all the same");
            assert_eq!(m.error(B), None);
            let notice = m.notice().expect("a notice").clone();
            drop(m);
            assert!(notice.error && notice.text.ends_with("The saved recording could not be removed yet. It is removed the next time Mythicshot starts."), "unfinished {unfinished}: {notice:?}");
            assert_eq!(notice.text == SAVED_NOT_REMOVED, !unfinished, "{notice:?}");
            assert!(text.replace('\n', " ").contains(SAVED_NOT_REMOVED), "unfinished {unfinished}:\n{text}");
            assert_eq!(has_line(&text, FINISH_DELETING), unfinished, "unfinished {unfinished}: the site's own files keep their block");
            assert!(state.unfinished().for_site(&s.base).len() == usize::from(unfinished));
            // It is no longer offered, and the next start finishes the removal, as the notice says.
            assert_eq!(store.list(), vec![]);
            assert!(saved.dir.join("frames.zst").is_file() && saved.dir.join(".removing").is_file());
            drop(held);
            publish_ui::tidy_saved_recordings(&store);
            assert!(!saved.dir.exists(), "unfinished {unfinished}: {:?}", files(store.root()));
        }
    }

    #[test]
    fn a_recording_saved_after_the_list_was_read_shows_without_asking_the_site_again() {
        let s = stub(|_, _| reply(200, LIST));
        let (_tmp, store) = temp_store();
        let (ctx, mut state, conn) = (crate::theme::test_context(), page().with_store(store.clone()), stub_conn(&s));
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
        assert!(!text.contains("saved on this computer"), "{text}");
        // A publish in an editor keeps its recording: the store counts the change, the page reads it.
        let saved = save_project(&store, &s.base, B, CH1, "Testing");
        let text = until(&ctx, &mut state, &conn, |m, _| !m.saved(B).is_empty());
        assert!(text.contains(&format!(" · recording saved on this computer ({})", publish_ui::size_text(saved.bytes))), "{text}");
        assert_eq!(s.lines().len(), 1, "the site was not asked again: {:?}", s.lines());
        // Removed again (its tour was deleted): the line goes, and Edit says so.
        store.remove_tour(&s.base, B).unwrap();
        let text = until(&ctx, &mut state, &conn, |m, _| m.saved(B).is_empty());
        assert!(!text.contains("saved on this computer"), "{text}");
        assert_eq!(s.lines().len(), 1);
    }

    #[test]
    fn a_recording_saved_while_its_tour_was_being_deleted_is_removed_when_it_shows_up() {
        // An upload to B is still saving its recording when B is deleted on the Tours page.
        let (ctx, state) = (crate::theme::test_context(), page());
        let file = written_to(&state.unfinished());
        let (s, _) = delete_stub(&file, 1, || reply(200, DELETED), |_| reply(404, NO_TOUR));
        let (_tmp, store) = temp_store();
        let kept = save_project(&store, &s.base, A, CH1, "Other tour");
        let mut state = state.with_store(store.clone());
        let conn = stub_conn(&s);
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        delete_b(&ctx, &mut state, &conn);
        assert_eq!(lock(&state.model).tours().len(), 1);
        // The upload's save lands now, for a tour the site said is gone.
        let late = save_project(&store, &s.base, B, CH1, "Testing");
        assert!(late.dir.exists());
        let deadline = Instant::now() + Duration::from_secs(20);
        while late.dir.exists() {
            assert!(Instant::now() < deadline, "the late recording was never removed: {:?}", files(store.root()));
            drawn(&ctx, &mut state, &conn);
            std::thread::sleep(Duration::from_millis(10));
        }
        // Only that one: a recording of a tour that was never deleted is not touched by it.
        assert_eq!(store.list(), vec![kept]);
        assert_eq!(s.lines().iter().filter(|l| *l == DELETE_B).count(), 1, "nothing more was asked of the site");
    }

    // ---- "No such tour" is also what the site says of another workspace's tour ----

    /// A page on the stub's site with its saved recordings in `store`, the list read: A, and B when `with_b`.
    fn listed_page(s: &Stub, store: &Store, epoch: u64, with_b: bool) -> (ToursState, Site) {
        let state = page().with_store(store.clone());
        let site = Site { server: s.base.clone(), epoch };
        read_into(&state, &site, with_b);
        (state, site)
    }

    /// The page now belongs to `site`, and its list is read: A, and B when `with_b`.
    fn read_into(state: &ToursState, site: &Site, with_b: bool) {
        let mut m = lock(&state.model);
        m.set_site(Some(site.clone()));
        let (generation, _) = m.begin_load().unwrap();
        let mut list = two();
        list.truncate(if with_b { 2 } else { 1 });
        m.finish_load(generation, Ok(list), Instant::now());
    }

    /// Press "Finish deleting" for B and run the job here, as its thread would.
    fn finish_deleting_b(state: &ToursState, s: &Stub) -> (Started, Result<Done, Failure>) {
        let job = lock(&state.model).finish_delete(&entry(&s.base, B, "Testing")).expect("the block offers it");
        let result = run_job(&state.model, &state.tools, &job);
        lock(&state.model).finish(&job, result.clone());
        (job, result)
    }

    #[test]
    fn finish_deleting_never_removes_a_saved_recording_on_no_such_tour_alone() {
        // A note from an earlier run of the app, and a token that may be another workspace's by
        // now: the site answers "no such tour" for a tour of another workspace, which may well exist.
        let s = stub(|req, _| if req.line == DELETE_B { reply(404, NO_TOUR) } else { reply(200, ONLY_A) });
        let (_tmp, store) = temp_store();
        let saved = saved_for_delete(&store, &s);
        let before = files(store.root());
        let (state, _) = listed_page(&s, &store, 1, false);
        state.unfinished().load(vec![entry(&s.base, B, "Testing")]);

        let (job, result) = finish_deleting_b(&state, &s);
        assert!(!job.listed, "the list does not hold the tour, so nothing says it is this workspace's");
        assert_eq!(result, Ok(Done::Deleted), "the site has nothing left to delete for this token");
        assert_eq!(s.lines().iter().filter(|l| *l == DELETE_B).count(), 1);
        // Not a file of any saved recording changed, and no removal was begun.
        let after = files(store.root());
        assert!(after == before, "a file of a saved recording changed");
        assert!(after.iter().all(|(name, _)| !name.ends_with(".removing")));
        assert_eq!(store.list().len(), saved.len());
        // Nor is the tour remembered as gone: reading the saved recordings later removes nothing.
        let (_, read, _) = read_saved(&state.tools, &s.base);
        assert_eq!(read.len(), 3, "both chapters of B and the one of A");
        assert!(files(store.root()) == before);
        assert_eq!(lock(&state.model).notice(), None);
        // They still open.
        assert!(crate::open_saved(Some(&store), &saved[0].key, &crate::settings::Settings::default()).is_ok());
    }

    #[test]
    fn finish_deleting_removes_the_saved_recording_once_the_site_answered_for_that_tour() {
        // The site still knows the delete: it answers for this very tour, so the tour was the token's.
        let answers: [fn(usize) -> Answer; 3] = [
            |_| reply(200, DELETED),
            |n| if n == 0 { reply(202, r#"{"status":"in_progress","done":5,"total":10}"#) } else { reply(404, NO_TOUR) },
            |n| if n == 0 { reply(500, r#"{"error":"cleanup_pending","detail":"the tour is deleted, but some of its files could not be removed yet; delete it again to retry"}"#) } else { reply(200, DELETED) },
        ];
        for (i, answer) in answers.into_iter().enumerate() {
            let s = stub(move |req, n| if req.line == DELETE_B { answer(n) } else { reply(200, ONLY_A) });
            let (_tmp, store) = temp_store();
            let [b1, b2, a1, elsewhere] = saved_for_delete(&store, &s);
            let (state, _) = listed_page(&s, &store, 1, false);
            state.unfinished().load(vec![entry(&s.base, B, "Testing")]);
            let (job, result) = finish_deleting_b(&state, &s);
            assert!(!job.listed);
            assert_eq!(result, Ok(Done::Deleted), "answer {i}");
            assert!(!b1.dir.exists() && !b2.dir.exists(), "answer {i}: {:?}", files(store.root()));
            let mut left = vec![a1, elsewhere];
            left.sort_by(|a, b| a.dir.cmp(&b.dir));
            assert_eq!(store.list(), left, "answer {i}: exactly that tour's, on that address");
        }
        // The same when the site only got as far as the tour itself: files are left there, and
        // the recordings go from this computer as after any delete that left files behind.
        let s = stub(|req, _| if req.line == DELETE_B { reply(202, r#"{"status":"in_progress","done":5,"total":10}"#) } else { reply(200, ONLY_A) });
        let (_tmp, store) = temp_store();
        saved_for_delete(&store, &s);
        let (state, _) = listed_page(&s, &store, 1, false);
        state.unfinished().load(vec![entry(&s.base, B, "Testing")]);
        let (_, result) = finish_deleting_b(&state, &s);
        assert!(matches!(result, Err(Failure::DeleteUnfinished(_))), "{result:?}");
        assert_eq!(store.for_tour(&s.base, B), vec![]);
        assert_eq!(store.list().len(), 2);
    }

    #[test]
    fn a_delete_from_the_row_removes_the_saved_recording_when_the_tour_was_already_gone() {
        // The list read with this token held the tour, so "no such tour" is about that tour:
        // it was deleted elsewhere in the meantime.
        let s = stub(|req, _| if req.line == DELETE_B { reply(404, NO_TOUR) } else { reply(200, ONLY_A) });
        let (_tmp, store) = temp_store();
        let [b1, b2, a1, elsewhere] = saved_for_delete(&store, &s);
        let (state, site) = listed_page(&s, &store, 1, true);
        let job = {
            let mut m = lock(&state.model);
            m.ask_delete(B);
            m.confirm_delete(B).unwrap()
        };
        assert_eq!(job, Started { site, id: B.into(), title: "Testing".into(), job: Job::Delete, listed: true });
        assert_eq!(run_job(&state.model, &state.tools, &job), Ok(Done::Deleted));
        assert!(!b1.dir.exists() && !b2.dir.exists());
        let mut left = vec![a1, elsewhere];
        left.sort_by(|a, b| a.dir.cmp(&b.dir));
        assert_eq!(store.list(), left);
    }

    #[test]
    fn finish_deleting_after_a_lost_answer_removes_the_recording_only_under_the_token_that_listed_the_tour() {
        for same_token in [true, false] {
            // The delete from the row gets no answer; the site did remove the tour, and says
            // "no such tour" from then on.
            let s = stub(|req, n| match (req.line.as_str(), n) {
                (DELETE_B, 0) => Answer::Drop,
                (DELETE_B, _) => reply(404, NO_TOUR),
                _ => reply(200, ONLY_A),
            });
            let (_tmp, store) = temp_store();
            let [b1, b2, a1, elsewhere] = saved_for_delete(&store, &s);
            let before = files(store.root());
            let (state, site) = listed_page(&s, &store, 1, true);
            let job = {
                let mut m = lock(&state.model);
                m.ask_delete(B);
                m.confirm_delete(B).unwrap()
            };
            let result = run_job(&state.model, &state.tools, &job);
            assert!(matches!(result, Err(Failure::Message(_))), "{result:?}");
            lock(&state.model).finish(&job, result);
            assert!(files(store.root()) == before, "a lost answer removes nothing");
            assert_eq!(state.unfinished().for_site(&s.base), vec![entry(&s.base, B, "Testing")], "the note is the way back");
            // The list is read again and no longer holds the tour: its block offers "Finish deleting".
            let now = if same_token { site.clone() } else { Site { epoch: site.epoch + 1, ..site.clone() } };
            read_into(&state, &now, false);
            let (again, result) = finish_deleting_b(&state, &s);
            assert!(!again.listed);
            assert_eq!(result, Ok(Done::Deleted), "same token {same_token}");
            if same_token {
                // This token's list held the tour in this session: "no such tour" is about it.
                assert!(!b1.dir.exists() && !b2.dir.exists(), "{:?}", files(store.root()));
                let mut left = vec![a1, elsewhere];
                left.sort_by(|a, b| a.dir.cmp(&b.dir));
                assert_eq!(store.list(), left);
            } else {
                // Another token was saved since, maybe another workspace's: nothing is removed.
                assert!(files(store.root()) == before, "a file of a saved recording changed");
                assert_eq!(store.list().len(), 4);
            }
        }
    }

    // ---- A tour that was said to be gone and is listed again ----

    #[test]
    fn a_tour_the_site_lists_again_is_no_longer_remembered_as_gone() {
        let s = stub(|req, n| match (req.line.as_str(), n) {
            // The first list is without B, every later one with it.
            ("GET /api/v1/tours", 0) => reply(200, ONLY_A),
            _ => reply(200, LIST),
        });
        let (_tmp, store) = temp_store();
        let state = page().with_store(store.clone());
        let site = Site { server: s.base.clone(), epoch: 1 };
        state.tools.gone.add(&s.base, B);
        state.tools.gone.add(&s.base, C);
        // While the site's list does not hold it, a recording that shows up for it is removed.
        let late = save_project(&store, &s.base, B, CH1, "Testing");
        let (_, saved, _, result) = read_list(&state.tools, &site);
        assert_eq!(result.map(|l| l.len()), Ok(1));
        assert!(!late.dir.exists() && saved.is_empty());
        // The list holds B again: it is on the site, and a recording saved for it stays.
        let kept = save_project(&store, &s.base, B, CH1, "Testing");
        let (_, saved, _, result) = read_list(&state.tools, &site);
        assert_eq!(result.map(|l| l.len()), Ok(2));
        assert_eq!(saved, vec![kept.clone()]);
        assert!(kept.dir.join("frames.zst").is_file());
        // For good, not only for that read: the entry is dropped, and only that tour's.
        assert_eq!(state.tools.gone.of(&s.base, [A, B, C].into_iter()), BTreeSet::from([C.to_string()]));
        let (_, saved, _) = read_saved(&state.tools, &s.base);
        assert_eq!(saved, vec![kept]);
    }

    #[test]
    fn a_list_that_could_not_be_read_forgets_no_gone_tour() {
        let s = stub(|_, _| reply(500, r#"{"error":"internal"}"#));
        let (_tmp, store) = temp_store();
        let state = page().with_store(store.clone());
        state.tools.gone.add(&s.base, B);
        let late = save_project(&store, &s.base, B, CH1, "Testing");
        let (_, saved, _, result) = read_list(&state.tools, &Site { server: s.base.clone(), epoch: 1 });
        assert!(result.is_err());
        assert!(!late.dir.exists() && saved.is_empty(), "the tour is still known to be gone");
        assert_eq!(state.tools.gone.of(&s.base, [B].into_iter()), BTreeSet::from([B.to_string()]));
        // Another address's list says nothing about this one's tours.
        state.tools.gone.listed("https://other.example.com", [B].into_iter());
        assert_eq!(state.tools.gone.of(&s.base, [B].into_iter()).len(), 1);
    }

    // ---- Sizes ----

    #[test]
    fn the_row_writes_a_size_as_preferences_does() {
        let line = |bytes: u64| saved_note(&[info(A, CH1, "Chapter one", bytes, None)], None).unwrap();
        // As before under a hundred megabytes.
        assert_eq!(line(860_160), "recording saved on this computer (840 KB)");
        assert_eq!(line(0), "recording saved on this computer (0 KB)");
        assert_eq!(line(1_048_576), "recording saved on this computer (1.0 MB)");
        assert_eq!(line(13_002_342), "recording saved on this computer (12.4 MB)");
        // Whole megabytes from a hundred, and gigabytes from one.
        assert_eq!(line(432_013_312), "recording saved on this computer (412 MB)");
        assert_eq!(line(1_073_741_823), "recording saved on this computer (1024 MB)");
        assert_eq!(line(1_073_741_824), "recording saved on this computer (1.0 GB)");
        assert_eq!(line(13_314_398_618), "recording saved on this computer (12.4 GB)");
        let two = [info(A, CH1, "One", 3_221_225_472, None), info(A, CH2, "Two", 2_147_483_648, None)];
        assert_eq!(saved_note(&two, None).as_deref(), Some("2 recordings saved on this computer (5.0 GB)"));
        // The very words Preferences uses for the same number of bytes.
        for bytes in [0, 860_160, 1_048_575, 13_002_342, 104_857_599, 432_013_312, 1_073_741_823, 1_073_741_824, 161_061_273_600] {
            let size = publish_ui::size_text(bytes);
            assert_eq!(line(bytes), format!("recording saved on this computer ({size})"));
            assert_eq!(project::usage_line(1, bytes, 0), format!("1 saved recording · {size}"));
        }
    }

    // ---- Publish changes ----

    /// A public tour whose live page is behind its draft.
    fn waiting(id: &str, title: &str) -> RemoteTour {
        RemoteTour { unpublished_changes: true, ..tour(id, title, true, 22) }
    }

    /// A draft a misbehaving site says has changes waiting.
    fn odd_draft(id: &str, title: &str) -> RemoteTour {
        RemoteTour { unpublished_changes: true, ..tour(id, title, false, 9) }
    }

    /// The list of a site of 0.1.6: A is public with changes waiting; B is a draft the site wrongly flags too.
    const LIST_WAITING: &str = r#"{"tours":[
        {"id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","title":"Customer onboarding","status":"published","slug":"abc123","url":"/t/abc123","chapters":2,"steps":22,"updated_at":1763424000000,"published_at":1763467200000,"unpublished_changes":true},
        {"id":"tour_bbbbbbbbbbbbbbbbbbbbbbbb","title":"Testing","status":"draft","slug":"def456","url":null,"chapters":1,"steps":9,"updated_at":1763424000000,"published_at":null,"unpublished_changes":true}]}"#;
    /// The same list once everything is published.
    const LIST_LIVE: &str = r#"{"tours":[
        {"id":"tour_aaaaaaaaaaaaaaaaaaaaaaaa","title":"Customer onboarding","status":"published","slug":"abc123","url":"/t/abc123","chapters":2,"steps":22,"updated_at":1763424000000,"published_at":1763467200000,"unpublished_changes":false},
        {"id":"tour_bbbbbbbbbbbbbbbbbbbbbbbb","title":"Testing","status":"draft","slug":"def456","url":null,"chapters":1,"steps":9,"updated_at":1763424000000,"published_at":null,"unpublished_changes":false}]}"#;
    const PUBLISH_A: &str = "POST /api/v1/tours/tour_aaaaaaaaaaaaaaaaaaaaaaaa/publish";
    const WAITING_NOTE: &str = " · changes waiting to be published";

    #[test]
    fn only_a_public_tour_with_changes_waiting_gets_publish_changes_after_edit() {
        use RowButton::*;
        assert_eq!(PublishChanges.label(), "Publish changes");
        let public = waiting(A, "A");
        assert_eq!(row_buttons(&public), [CopyLink, Open, Edit, PublishChanges, Rename, Unpublish, Delete]);
        assert_eq!(row_buttons(&public).iter().map(|b| b.label()).collect::<Vec<_>>(), ["Copy link", "Open", "Edit", "Publish changes", "Rename", "Unpublish", "Delete"]);
        // An older site's row without a link: still directly after Edit.
        let unlinked = RemoteTour { link: None, ..public.clone() };
        assert_eq!(row_buttons(&unlinked), [Edit, PublishChanges, Rename, Unpublish, Delete]);
        // A public tour with nothing waiting is as it was.
        assert_eq!(row_buttons(&tour(A, "A", true, 22)), [CopyLink, Open, Edit, Rename, Unpublish, Delete]);
        // A draft keeps its own Publish and never gets the button, whatever the site says.
        assert_eq!(row_buttons(&odd_draft(B, "B")), [Publish, Edit, Rename, Delete]);
        assert_eq!(row_buttons(&tour(B, "B", false, 9)), [Publish, Edit, Rename, Delete]);
    }

    #[test]
    fn the_detail_line_says_changes_are_waiting_after_the_date_and_before_the_saved_recording() {
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap();
        let mut t = waiting(A, "Customer onboarding");
        t.published_at = Some(now.timestamp_millis());
        assert_eq!(detail_line(&t, &[], None, &now), "2 chapters · 22 steps · published 18 Nov · changes waiting to be published");
        let one = [info(A, CH1, "Chapter one", mb(12.4), None)];
        assert_eq!(detail_line(&t, &one, None, &now), "2 chapters · 22 steps · published 18 Nov · changes waiting to be published · recording saved on this computer (12.4 MB)");
        // A site that gives no date: after the counts.
        t.published_at = None;
        assert_eq!(detail_line(&t, &[], None, &now), "2 chapters · 22 steps · changes waiting to be published");
        // Nothing waiting, and a draft the site flags: the line is as it always was.
        t.unpublished_changes = false;
        assert_eq!(detail_line(&t, &one, None, &now), "2 chapters · 22 steps · recording saved on this computer (12.4 MB)");
        let mut d = odd_draft(B, "Testing");
        d.updated_at = Some(now.timestamp_millis());
        assert_eq!(detail_line(&d, &[], None, &now), "2 chapters · 9 steps · updated 18 Nov");
    }

    #[test]
    fn publish_changes_runs_the_publish_job_once_and_only_where_the_button_shows() {
        // Nothing to publish: a public tour without the flag, a draft with it, a tour that is not there.
        let mut m = loaded(vec![tour(A, "Customer onboarding", true, 22), odd_draft(B, "Testing")]);
        assert_eq!((m.start_publish_changes(A), m.start_publish_changes(B), m.start_publish_changes(C)), (None, None, None));
        assert_eq!((m.work(A), m.work(B)), (None, None));
        // With changes waiting: the very job a draft's Publish runs, with its progress, and once.
        let mut m = loaded(vec![waiting(A, "Customer onboarding"), tour(B, "Testing", false, 9)]);
        m.ask_delete(A);
        let job = m.start_publish_changes(A).unwrap();
        let draft_job = m.start_publish(B).unwrap();
        assert_eq!((&job.job, &draft_job.job), (&Job::Publish, &Job::Publish));
        assert_eq!((job.id.as_str(), job.listed), (A, true));
        assert_eq!((m.work(A), m.work(B)), (Some(Work::Publishing(None)), Some(Work::Publishing(None))));
        assert_eq!(m.edit(), &Edit::None, "no question is asked, and an open one on that tour closes");
        assert_eq!(m.start_publish_changes(A), None, "a second press starts nothing");
        m.progress(A, Work::Publishing(Some((3, 22))));
        assert_eq!(work_message(m.work(A).unwrap()), "Publishing… 3 of 22 images");
        // A draft's own Publish never fits a public tour, with or without changes waiting.
        let mut m = loaded(vec![waiting(A, "Customer onboarding")]);
        assert_eq!(m.start_publish(A), None);
    }

    #[test]
    fn publish_changes_does_not_start_while_another_job_runs_on_that_tour() {
        let fresh = || loaded(vec![waiting(A, "Customer onboarding"), tour(B, "Testing", false, 9)]);
        let starts: [(&str, fn(&mut Model) -> Option<Started>); 4] = [
            ("a rename", |m| {
                m.start_rename(A);
                *m.rename_text().unwrap() = "Another name".into();
                m.save_rename()
            }),
            ("a delete", |m| {
                m.ask_delete(A);
                m.confirm_delete(A)
            }),
            ("an unpublish", |m| m.start_unpublish(A)),
            ("another publish", |m| m.start_publish_changes(A)),
        ];
        for (what, start) in starts {
            let mut m = fresh();
            let running = start(&mut m).unwrap_or_else(|| panic!("{what} starts"));
            assert_eq!(m.start_publish_changes(A), None, "while {what} runs");
            assert_eq!(m.work(A).map(work_message), Some(work_message(match running.job {
                Job::Rename(_) => Work::Renaming,
                Job::Delete => Work::Deleting(None),
                Job::Unpublish => Work::Unpublishing,
                Job::Publish => Work::Publishing(None),
            })), "the running job keeps its own progress");
            // Once it ended without changing the tour, the button works again.
            m.finish(&running, Err(Failure::Message("It did not work.".into())));
            assert!(m.start_publish_changes(A).is_some(), "after {what}");
        }
    }

    #[test]
    fn a_publish_of_changes_that_worked_takes_the_flag_away_and_one_that_failed_keeps_it() {
        let mut m = loaded(vec![waiting(A, "Customer onboarding")]);
        let job = m.start_publish_changes(A).unwrap();
        m.finish(&job, Err(Failure::Message("Could not publish the tour: the server answered HTTP 500.".into())));
        assert_eq!(m.error(A), Some("Could not publish the tour: the server answered HTTP 500."));
        assert!(m.tours()[0].unpublished_changes && row_buttons(&m.tours()[0]).contains(&RowButton::PublishChanges));
        // Pressing again clears the error line and runs again.
        let job = m.start_publish_changes(A).unwrap();
        assert_eq!(m.error(A), None);
        m.finish(&job, Ok(Done::Published(Some("https://tours.example.com/t/aaaaaa".into()))));
        let t = &m.tours()[0];
        assert!(t.is_public() && !t.unpublished_changes && t.link.is_some());
        assert_eq!(row_buttons(t), [RowButton::CopyLink, RowButton::Open, RowButton::Edit, RowButton::Rename, RowButton::Unpublish, RowButton::Delete]);
        assert_eq!(m.start_publish_changes(A), None);
        assert!(m.wants_load(Instant::now()), "the list is read again");
        // Unpublishing a tour with changes waiting leaves a plain draft.
        let mut m = loaded(vec![waiting(A, "Customer onboarding")]);
        let job = m.start_unpublish(A).unwrap();
        m.finish(&job, Ok(Done::Unpublished));
        assert_eq!(row_buttons(&m.tours()[0]), [RowButton::Publish, RowButton::Edit, RowButton::Rename, RowButton::Delete]);
    }

    #[test]
    fn pressing_publish_changes_publishes_that_tour_with_progress_and_the_button_and_note_go() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let live = Arc::new(AtomicBool::new(false));
        let done = live.clone();
        let s = stub(move |req, seen| match req.line.as_str() {
            "GET /api/v1/tours" => reply(200, if done.load(Ordering::SeqCst) { LIST_LIVE } else { LIST_WAITING }),
            PUBLISH_A if seen == 0 => reply(202, r#"{"status":"in_progress","publish_id":"pub_0123456789abcdef01234567","done":1,"total":4}"#),
            PUBLISH_A => {
                // Long enough for the page to draw the count.
                std::thread::sleep(Duration::from_millis(300));
                done.store(true, Ordering::SeqCst);
                reply(200, r#"{"status":"published","steps":22,"slug":"abc123","url":"/t/abc123"}"#)
            }
            _ => reply(500, r#"{"error":"unexpected"}"#),
        });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
        // The button and the note on the public tour only; the flagged draft is a plain draft.
        assert_eq!(text.lines().filter(|l| *l == "Publish changes").count(), 1, "{text}");
        assert_eq!(text.lines().filter(|l| *l == "Publish").count(), 1, "{text}");
        let flat = text.replace('\n', " ");
        assert_eq!(flat.matches(WAITING_NOTE).count(), 1, "{text}");
        assert!(flat.contains(&format!("2 chapters · 22 steps · published {}{WAITING_NOTE}", day_label(1_763_467_200_000, &Local::now()).unwrap())), "{text}");
        assert!(flat.contains("1 chapter · 9 steps · updated ") && !flat.contains(&format!("updated {}{WAITING_NOTE}", day_label(1_763_424_000_000, &Local::now()).unwrap())), "{text}");
        // The row's buttons in order, on one line: Copy link, Open, Edit, Publish changes, Rename, Unpublish, Delete.
        let at = |state: &mut ToursState, word: &str| places(&ctx, state, &conn, word)[0];
        let row: Vec<egui::Pos2> = ["Copy link", "Open", "Edit", "Publish changes", "Rename", "Unpublish", "Delete"].iter().map(|w| at(&mut state, w)).collect();
        assert!(row.windows(2).all(|w| w[0].x < w[1].x && (w[0].y - w[1].y).abs() < 1.0), "{row:?}");
        assert!(row[6].x < 590.0, "the row fits the page: {row:?}");
        assert!(s.lines().iter().all(|l| l == "GET /api/v1/tours"), "looking sends nothing: {:?}", s.lines());

        // One press: no question, the job runs at once.
        assert_eq!(click(&ctx, &mut state, &conn, row[3]), None);
        {
            let m = lock(&state.model);
            assert_eq!(m.edit(), &Edit::None);
            assert!(m.work(A).is_some() || s.lines().iter().any(|l| l == PUBLISH_A), "the publish started");
        }
        // The row's progress, with the site's count, in place of that row's buttons.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (text, _) = drawn(&ctx, &mut state, &conn);
            if has_line(&text, "Publishing… 1 of 4 images") {
                assert!(!has_line(&text, "Publish changes") && !has_line(&text, "Copy link") && !has_line(&text, "Unpublish"), "{text}");
                assert!(has_line(&text, "Publish"), "the other row stays usable:\n{text}");
                break;
            }
            assert!(Instant::now() < deadline, "the count never showed:\n{text}");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Done: the list is read again, and the button and the note are gone.
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(A).is_none() && !m.is_loading() && !m.wants_load(Instant::now()) && m.tours().len() == 2);
        assert!(!text.contains("Publish changes") && !text.replace('\n', " ").contains(WAITING_NOTE), "{text}");
        assert!(has_line(&text, "Copy link") && has_line(&text, "Unpublish") && has_line(&text, "Public") && !text.contains("Could not"), "{text}");
        {
            let m = lock(&state.model);
            assert!(m.tours().iter().all(|t| !t.unpublished_changes) && m.tours()[0].is_public() && m.tours()[0].published_at == Some(1_763_467_200_000));
            assert_eq!(m.tours()[0].link, Some(format!("{}/t/abc123", s.base)));
        }
        // Only publish calls for that tour were sent besides the list: no upload, nothing for the draft.
        let reqs = s.requests();
        let sent: Vec<&crate::publish::tests::Req> = reqs.iter().filter(|r| r.line != "GET /api/v1/tours").collect();
        assert_eq!(sent.iter().map(|r| r.line.as_str()).collect::<Vec<_>>(), [PUBLISH_A; 2]);
        assert!(sent[0].body.is_empty());
        assert_eq!(String::from_utf8_lossy(&sent[1].body), r#"{"publish_id":"pub_0123456789abcdef01234567"}"#);
        assert!(reqs.iter().all(|r| !r.line.contains("upload") && !r.line.contains("dp_")) && !text.contains("dp_"));
        assert!(reqs.iter().filter(|r| r.line == "GET /api/v1/tours").count() >= 2, "the list was read again");
    }

    #[test]
    fn a_publish_of_changes_shows_what_a_draft_publish_shows_while_it_runs() {
        // The row of a draft being published offers its progress and nothing to press (there is
        // no Cancel on a Tours row); publishing changes is the same job and shows the same.
        let s = stub(|req, _| if req.line == "GET /api/v1/tours" { reply(200, LIST_WAITING) } else { reply(500, r#"{"error":"unexpected"}"#) });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
        {
            // Both jobs are held as running, without their threads: the page draws what the model says.
            let mut m = lock(&state.model);
            assert!(m.start_publish_changes(A).is_some() && m.start_publish(B).is_some());
            m.progress(A, Work::Publishing(Some((2, 9))));
            m.progress(B, Work::Publishing(Some((2, 9))));
        }
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert_eq!(text.lines().filter(|l| *l == "Publishing… 2 of 9 images").count(), 2, "{text}");
        for word in ["Publish changes", "Publish", "Cancel", "Edit", "Rename", "Unpublish", "Delete", "Copy link", "Open"] {
            assert!(!has_line(&text, word), "a row that is publishing offers no {word:?}:\n{text}");
        }
        // The note stays on the detail line until the site says the changes are live.
        assert_eq!(text.replace('\n', " ").matches(WAITING_NOTE).count(), 1, "{text}");
        assert!(s.lines().iter().all(|l| l == "GET /api/v1/tours"));
    }

    #[test]
    fn a_publish_of_changes_that_fails_leaves_the_button_with_the_error_line() {
        let s = stub(|req, _| match req.line.as_str() {
            "GET /api/v1/tours" => reply(200, LIST_WAITING),
            _ => reply(500, r#"{"error":"server_error"}"#),
        });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
        let button = places(&ctx, &mut state, &conn, "Publish changes");
        assert_eq!(button.len(), 1);
        assert_eq!(click(&ctx, &mut state, &conn, button[0]), None);
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(A).is_none() && m.error(A).is_some() && !m.is_loading() && !m.wants_load(Instant::now()));
        let error = lock(&state.model).error(A).unwrap().to_string();
        assert!(error.starts_with("Could not publish the tour: ") && error.contains("HTTP 500"), "{error}");
        assert!(text.replace('\n', " ").contains(&error), "{text}");
        assert!(has_line(&text, "Publish changes") && text.replace('\n', " ").contains(WAITING_NOTE), "the button and the note are still there:\n{text}");
        assert_eq!(s.lines().iter().filter(|l| *l == PUBLISH_A).count(), 1, "a failed call is not sent again");
        assert!(s.lines().iter().all(|l| l == "GET /api/v1/tours" || l == PUBLISH_A), "{:?}", s.lines());
    }

    #[test]
    fn a_site_that_does_not_say_whether_changes_wait_shows_no_publish_changes() {
        // The list of a site of 0.1.5: no such field on any row.
        let s = stub(|req, _| if req.line == "GET /api/v1/tours" { reply(200, LIST) } else { reply(500, r#"{"error":"unexpected"}"#) });
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        let text = until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 2);
        assert!(!text.contains("Publish changes") && !text.contains("changes waiting"), "{text}");
        for (word, times) in [("Copy link", 1), ("Open", 1), ("Unpublish", 1), ("Publish", 1), ("Edit", 2), ("Rename", 2), ("Delete", 2)] {
            assert_eq!(text.lines().filter(|l| *l == word).count(), times, "{word:?} in:\n{text}");
        }
        let mut m = lock(&state.model);
        assert!(m.tours().iter().all(|t| !t.unpublished_changes));
        assert_eq!((m.start_publish_changes(A), m.start_publish_changes(B)), (None, None));
    }

    // ---- A recording kept as a video: the compressing note, and Edit that expands ----

    use mythicshot_recording::CodecError;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    /// What the store's list says of a saved chapter whose recording is kept as a video.
    fn video_info(tour: &str, chapter: &str, title: &str, bytes: u64) -> ProjectInfo {
        ProjectInfo { form: Form::Video, convert: project::ConvertState::Done, dir: PathBuf::from(tour).join(chapter), ..info(tour, chapter, title, bytes, None) }
    }

    /// The two listed tours and a third, C, with one chapter.
    fn three() -> Vec<RemoteTour> {
        let mut list = listed();
        list.push(RemoteTour { chapters: 1, ..tour(C, "Third", false, 4) });
        list
    }

    /// Save a small real recording and make its project the video form by hand, as a swap leaves
    /// it: the video beside a format-2 project file and index, and no frames file. No conversion
    /// runs, so the one conversion of the process is never taken.
    pub(crate) fn video_project(store: &Store, server: &str, tour: &str, chapter: &str, title: &str) -> ProjectInfo {
        const VIDEO: &[u8] = b"a video of the tours tests";
        let saved = save_project(store, server, tour, chapter, title);
        let rec = mythicshot_recording::Recording::open_saved(&saved.dir).unwrap();
        rec.write_video_index(&saved.dir, VIDEO.len() as u64).unwrap();
        std::fs::write(saved.dir.join("frames.mkv"), VIDEO).unwrap();
        let mut file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(saved.dir.join("project.json")).unwrap()).unwrap();
        file["format"] = serde_json::json!(2);
        file["store"] = serde_json::json!({"form": "video", "video_bytes": VIDEO.len(), "lossless_bytes": saved.frames_bytes});
        std::fs::write(saved.dir.join("project.json"), file.to_string()).unwrap();
        std::fs::remove_file(saved.dir.join("frames.zst")).unwrap();
        let listed = store.for_site(server).into_iter().find(|p| p.key == saved.key).expect("the project is listed");
        assert_eq!((listed.form, listed.problem, listed.frames_bytes), (Form::Video, None, saved.frames_bytes), "the project made by hand is a sound video project");
        listed
    }

    /// Reads any video back as the pictures its index asks for, each filled with its number.
    #[derive(Default)]
    pub(crate) struct FakeCodec {
        /// Called before every picture is handed over, with its number.
        pub during: Option<Box<dyn Fn(usize) + Send + Sync>>,
        /// Reading ends with this error before the first picture.
        pub fails: Option<fn() -> CodecError>,
        /// How often a read was asked for.
        pub decodes: AtomicUsize,
    }

    impl VideoCodec for FakeCodec {
        fn id(&self) -> &str {
            "a-codec-of-the-tours-tests"
        }

        fn encode(&self, _: (u32, u32), _: usize, _: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, _: &Path, _: u64, _: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            Err(CodecError::Failed { exit: None })
        }

        fn decode(&self, _: &Path, size: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
            self.decodes.fetch_add(1, Ordering::SeqCst);
            if let Some(why) = self.fails {
                return Err(why());
            }
            for i in 0..frames {
                if !keep_going(i) {
                    return Err(CodecError::Cancelled);
                }
                if let Some(during) = &self.during {
                    during(i);
                }
                each(i, &vec![i as u8; size.0 as usize * size.1 as usize * 4]).map_err(CodecError::Caller)?;
            }
            Ok(())
        }
    }

    /// A page that lists the three tours and holds these saved recordings, with no site and no
    /// folder behind it.
    fn page_with(saved: Vec<ProjectInfo>) -> ToursState {
        let state = page();
        *lock(&state.model) = loaded_with(three(), saved);
        state
    }

    /// No room is asked about: the disk does not say.
    fn any_room(_: &Path) -> Option<u64> {
        None
    }

    /// Run the body of the opening thread here, and hand back the one thing it reported.
    fn opened(open: &VideoOpen<'_>, key: &ProjectKey, rows: &OpenRows) -> Result<Loaded, LoadError> {
        let (tx, rx) = mpsc::channel();
        open_video(open, key, rows, |result| tx.send(result).unwrap());
        let result = rx.try_recv().expect("the open reports before it returns");
        assert!(rx.try_recv().is_err(), "it reports once");
        result
    }

    #[test]
    fn a_tour_whose_recording_is_being_compressed_says_so_inside_the_brackets() {
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap();
        let mut t = tour(A, "Customer onboarding", true, 22);
        t.published_at = Some(now.timestamp_millis());
        let one = [info(A, CH1, "Chapter one", mb(12.4), None)];
        let dir = one[0].dir.clone();
        assert_eq!(saved_note(&one, Some(&dir)).as_deref(), Some("recording saved on this computer (12.4 MB, compressing…)"));
        assert_eq!(detail_line(&t, &one, Some(&dir), &now), "2 chapters · 22 steps · published 18 Nov · recording saved on this computer (12.4 MB, compressing…)");
        // Another project is being compressed, or none: the line is as it always was.
        assert_eq!(saved_note(&one, Some(Path::new("elsewhere"))).as_deref(), Some("recording saved on this computer (12.4 MB)"));
        assert_eq!(saved_note(&one, None).as_deref(), Some("recording saved on this computer (12.4 MB)"));
        // A recording with a problem is not counted, so nothing is said of it either.
        let broken = [info(A, CH1, "Chapter one", mb(12.4), Some(Problem::Damaged))];
        assert_eq!(saved_note(&broken, Some(&dir)), None);
        assert_eq!(saved_note(&[], Some(&dir)), None);

        // The folder is stored with the saved recordings when they are read, and goes with the next read.
        let mut m = loaded(two());
        assert_eq!(m.converting(), None);
        m.apply_saved(&site(), 1, one.to_vec(), Some(dir.clone()));
        assert_eq!(m.converting(), Some(dir.as_path()));
        // A read for another site, and one older than what the page holds, change nothing.
        m.apply_saved(&Site { server: "https://other.example.com".into(), epoch: 1 }, 2, vec![], None);
        m.apply_saved(&site(), 0, vec![], None);
        assert_eq!((m.converting(), m.saved(A).len()), (Some(dir.as_path()), 1));
        m.finish_saved(&site(), 2, one.to_vec(), None);
        assert_eq!(m.converting(), None);
        // Another site's page holds none of it.
        m.apply_saved(&site(), 3, one.to_vec(), Some(dir.clone()));
        m.set_site(Some(Site { server: "https://other.example.com".into(), epoch: 1 }));
        assert_eq!(m.converting(), None);

        // The read itself asks which project is being compressed, on its own thread, once.
        let (_tmp, store) = temp_store();
        let mut state = ToursState::with_token(|_| Err("no token is asked for in this test".into())).with_store(store);
        let (asked, being) = (Arc::new(AtomicUsize::new(0)), dir.clone());
        let count = asked.clone();
        state.tools.converting = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Some(being.clone())
        });
        assert_eq!(read_saved(&state.tools, &site().server).2, Some(dir.clone()));
        let (_, _, converting, _) = read_list(&state.tools, &Site { server: "http://127.0.0.1:9".into(), epoch: 1 });
        assert_eq!((converting, asked.load(Ordering::SeqCst)), (Some(dir), 2));
    }

    #[test]
    fn a_tour_with_two_recordings_says_compressing_when_either_is() {
        let two = [info(A, CH1, "Chapter one", mb(12.4), None), info(A, CH2, "Chapter two", mb(18.6), None)];
        for p in &two {
            assert_eq!(saved_note(&two, Some(&p.dir)).as_deref(), Some("2 recordings saved on this computer (31.0 MB, compressing…)"));
        }
        assert_eq!(saved_note(&two, None).as_deref(), Some("2 recordings saved on this computer (31.0 MB)"));
        // The one being compressed has a problem by now: only the other is counted, and it is not being compressed.
        let mixed = [ProjectInfo { problem: Some(Problem::FramesMissing), ..two[0].clone() }, two[1].clone()];
        assert_eq!(saved_note(&mixed, Some(&two[0].dir)).as_deref(), Some("recording saved on this computer (18.6 MB)"));
        // The sizes of the plan's own example.
        let plan = [info(A, CH1, "Chapter one", mb(172.0), None), info(A, CH2, "Chapter two", mb(32.0), None)];
        let size = publish_ui::size_text(mb(204.0));
        assert_eq!(saved_note(&plan, Some(&plan[1].dir)), Some(format!("2 recordings saved on this computer ({size}, compressing…)")));
    }

    #[test]
    fn a_video_recording_reads_like_any_saved_recording_in_the_row() {
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap();
        let mut t = tour(A, "Customer onboarding", true, 22);
        t.published_at = None;
        let video = [video_info(A, CH1, "Chapter one", mb(12.4))];
        assert_eq!(saved_note(&video, None).as_deref(), Some("recording saved on this computer (12.4 MB)"));
        assert_eq!(detail_line(&t, &video, None, &now), "2 chapters · 22 steps · recording saved on this computer (12.4 MB)");
        // Beside one kept as frames it is counted like it.
        let both = [video[0].clone(), info(A, CH2, "Chapter two", mb(18.6), None)];
        assert_eq!(saved_note(&both, None).as_deref(), Some("2 recordings saved on this computer (31.0 MB)"));
        // The buttons and the questions are the same.
        assert_eq!(row_buttons(&t), row_buttons(&tour(A, "Customer onboarding", true, 22)));
        assert_eq!(delete_question(&t, video.len()), delete_question(&t, 1));
        assert_eq!(chapter_label(&video[0]), "Chapter one");
    }

    #[test]
    fn every_new_reason_has_its_own_sentence() {
        assert_eq!(load_sentence(&LoadError::NeedsFfmpeg), Some("This tour's recording is stored as video, and ffmpeg was not found on this computer. Install ffmpeg and start Mythicshot again to edit it. Nothing was deleted."));
        assert_eq!(load_sentence(&LoadError::VideoDamaged), Some("This tour's recording could not be read back from its video, so it can't be opened here. The recording is left as it is, and the tour on the site is not affected."));
        assert_eq!(load_sentence(&LoadError::NoRoom), Some("There is not enough room on this computer to open this tour's recording. Free some space and choose Edit again."));
        // A cancel is what the user asked for: nothing is said.
        assert_eq!(load_sentence(&LoadError::Cancelled), None);
        assert_eq!(OPENING_ANOTHER, "Another recording is being opened. Wait for it to finish, or cancel it.");
        assert_eq!((NEEDS_FFMPEG, VIDEO_DAMAGED, NO_ROOM), (load_sentence(&LoadError::NeedsFfmpeg).unwrap(), load_sentence(&LoadError::VideoDamaged).unwrap(), load_sentence(&LoadError::NoRoom).unwrap()));
        // The earlier reasons keep theirs.
        assert_eq!(load_sentence(&LoadError::Damaged("anything".into())), Some(SAVED_DAMAGED));
        assert_eq!(load_sentence(&LoadError::Newer), Some("This tour's recording was saved by a newer version of Mythicshot. Update Mythicshot to edit it."));
        let all = [NOT_SAVED, SAVED_DAMAGED, SAVED_NEWER, SAVED_FRAMES_MISSING, ALREADY_OPEN, NEEDS_FFMPEG, VIDEO_DAMAGED, NO_ROOM, OPENING_ANOTHER];
        assert_eq!(all.iter().collect::<BTreeSet<_>>().len(), all.len());
    }

    #[test]
    fn edit_of_a_video_recording_without_ffmpeg_says_so_and_asks_for_nothing() {
        let video = video_info(B, CH1, "Testing", mb(32.2));
        let frames = info(C, CH1, "Third", mb(3.0), None);
        let chapter = video_info(A, CH1, "Getting started", mb(8.0));
        let mut m = loaded_with(three(), vec![video.clone(), frames.clone(), chapter.clone()]);
        m.set_ffmpeg(FfmpegState::NotFound);
        assert_eq!(m.ask_edit(B), None);
        assert_eq!(m.error(B), Some(NEEDS_FFMPEG));
        assert_eq!((m.edit(), m.work(B), m.is_opening()), (&Edit::None, None, false));
        // A recording kept as frames needs no ffmpeg.
        assert_eq!(m.ask_edit(C), Some(frames.key.clone()));
        assert_eq!(m.error(C), None);
        // A tour with several chapters asks which first, and says it when the chapter is chosen.
        assert_eq!(m.ask_edit(A), None);
        assert_eq!((m.edit(), m.error(A)), (&Edit::ChoosingChapter { id: A.into() }, None));
        assert_eq!(m.choose_chapter(CH1), None);
        assert_eq!((m.edit(), m.error(A)), (&Edit::None, Some(NEEDS_FFMPEG)));
        // While ffmpeg is still being looked for, and once it is found, the recording is asked for: opening it decides.
        for state in [FfmpegState::Checking, FfmpegState::Found] {
            m.set_ffmpeg(state);
            assert_eq!(m.ask_edit(B), Some(video.key.clone()), "{state:?}");
            assert_eq!(m.error(B), None, "{state:?}: the sentence goes with the next try");
            assert_eq!(m.ask_edit(A), None);
            assert_eq!(m.choose_chapter(CH1), Some(chapter.key.clone()), "{state:?}");
        }
    }

    #[test]
    fn opening_shows_its_percentage_and_cancel_and_hides_the_other_buttons() {
        assert_eq!(work_message(Work::Opening(None)), "Opening the recording…");
        assert_eq!(work_message(Work::Opening(Some((100, 250)))), "Opening the recording… 40%");
        assert_eq!(work_message(Work::Opening(Some((99, 248)))), "Opening the recording… 39%");
        assert_eq!(work_message(Work::Opening(Some((0, 248)))), "Opening the recording… 0%");
        assert_eq!(work_message(Work::Opening(Some((248, 248)))), "Opening the recording… 100%");
        assert_eq!(work_message(Work::Opening(Some((300, 248)))), "Opening the recording… 100%");
        assert_eq!(work_message(Work::Opening(Some((0, 0)))), "Opening the recording…");
        assert_eq!(work_message(Work::Opening(Some((u32::MAX, u32::MAX)))), "Opening the recording… 100%");

        let video = video_info(B, CH1, "Testing", mb(32.2));
        let mut state = page_with(vec![video.clone()]);
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(state.begin_open(&video.key, cancel.clone()));
        assert_eq!(lock(&state.model).work(B), Some(Work::Opening(None)));
        let rows = state.open_rows();
        rows.progress(&video.key, 100, 250);
        assert_eq!(lock(&state.model).work(B), Some(Work::Opening(Some((100, 250)))));
        // Progress of anything else is not this row's.
        rows.progress(&key(&site().server, C, CH1), 5, 10);
        rows.progress(&key("https://other.example.com", B, CH1), 5, 10);
        rows.progress(&key(&site().server, B, CH2), 5, 10);
        let shown = |id: &str| lock(&state.model).work(id);
        assert_eq!((shown(B), shown(C)), (Some(Work::Opening(Some((100, 250)))), None));

        let (ctx, conn) = (crate::theme::test_context(), conn(TokenStatus::Saved));
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert!(has_line(&text, "Opening the recording… 40%"), "{text}");
        assert_eq!(text.lines().filter(|l| *l == "Cancel").count(), 1, "{text}");
        // The row that opens shows nothing else to press; the other rows keep theirs.
        for (word, times) in [("Edit", 2), ("Rename", 2), ("Delete", 2), ("Publish", 1)] {
            assert_eq!(text.lines().filter(|l| *l == word).count(), times, "{word:?} in:\n{text}");
        }
        // Cancel on the row stops it.
        let at = places(&ctx, &mut state, &conn, "Cancel");
        assert_eq!(click(&ctx, &mut state, &conn, at[0]), None);
        assert!(cancel.load(Ordering::SeqCst));
        let (text, _) = drawn(&ctx, &mut state, &conn);
        assert!(!text.contains("Opening the recording") && !has_line(&text, "Cancel"), "{text}");
        assert_eq!(text.lines().filter(|l| *l == "Edit").count(), 3, "{text}");
    }

    #[test]
    fn a_second_edit_while_one_recording_opens_is_refused_with_its_sentence() {
        let (first, second, frames) = (video_info(B, CH1, "Testing", mb(32.2)), video_info(C, CH1, "Third", mb(8.0)), info(A, CH1, "Getting started", mb(3.0), None));
        let mut m = loaded_with(three(), vec![first.clone(), second.clone(), frames.clone()]);
        let (one, other) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        assert_eq!(m.ask_edit(B), Some(first.key.clone()));
        assert!(m.begin_open(&first.key, one.clone()));
        assert!(m.is_opening());
        // Edit on another recording kept as a video: refused on its row, and nothing is asked of the app.
        assert_eq!(m.ask_edit(C), None);
        assert_eq!((m.error(C), m.work(C)), (Some(OPENING_ANOTHER), None));
        // The app is refused all the same, should it ask.
        m.errors.clear();
        assert!(!m.begin_open(&second.key, other.clone()));
        assert_eq!((m.error(C), m.work(C)), (Some(OPENING_ANOTHER), None));
        assert!(!m.begin_open(&first.key, other.clone()), "nor the same one twice");
        // The one that opens is not disturbed, and neither flag was touched.
        assert_eq!((m.work(B), m.error(B), one.load(Ordering::SeqCst), other.load(Ordering::SeqCst)), (Some(Work::Opening(None)), None, false, false));
        // Its own row has no Edit to press.
        assert_eq!(m.ask_edit(B), None);
        assert_eq!(m.error(B), None);
        // A recording kept as frames opens at once, as ever: the chapter question, then its key.
        assert_eq!(m.ask_edit(A), None);
        assert_eq!(m.choose_chapter(CH1), Some(frames.key.clone()));
        // Something else ended: the open goes on.
        m.open_ended(&second.key);
        assert!(m.is_opening() && m.work(B).is_some());
        // Once it has ended, the other opens.
        m.open_ended(&first.key);
        assert!(!m.is_opening() && m.work(B).is_none());
        assert_eq!(m.ask_edit(C), Some(second.key.clone()));
        assert_eq!(m.error(C), None);
        assert!(m.begin_open(&second.key, other));
        assert_eq!(m.work(C), Some(Work::Opening(None)));
    }

    #[test]
    fn cancel_sets_the_flag_and_clears_the_row() {
        let video = video_info(B, CH1, "Testing", mb(32.2));
        let mut m = loaded_with(three(), vec![video.clone()]);
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(m.begin_open(&video.key, cancel.clone()));
        m.open_progress(&video.key, 3, 9);
        // Cancel on another row is none of this open's business.
        m.cancel_open(A);
        m.cancel_open("tour_that_is_not_listed");
        assert_eq!((cancel.load(Ordering::SeqCst), m.work(B)), (false, Some(Work::Opening(Some((3, 9))))));
        m.cancel_open(B);
        assert!(cancel.load(Ordering::SeqCst));
        assert_eq!((m.work(B), m.error(B), m.notice()), (None, None, None));
        // Late progress does not bring the row back.
        m.open_progress(&video.key, 4, 9);
        assert_eq!(m.work(B), None);
        // The work is still letting go: no other recording opens until it has ended.
        assert!(m.is_opening());
        assert!(!m.begin_open(&key(&site().server, C, CH1), Arc::default()));
        m.open_ended(&video.key);
        assert!(!m.is_opening());
        // A cancel with nothing opening does nothing.
        m.cancel_open(B);
        assert_eq!(m.work(B), None);
    }

    #[test]
    fn a_refresh_that_removes_the_tour_keeps_the_open_running_and_drops_its_row_state() {
        let video = video_info(B, CH1, "Testing", mb(32.2));
        let mut m = loaded_with(three(), vec![video.clone()]);
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(m.begin_open(&video.key, cancel.clone()));
        m.open_progress(&video.key, 3, 9);
        // The list is read again and the tour is gone from it.
        reload(&mut m, vec![tour(A, "Customer onboarding", true, 22)]);
        assert_eq!(m.work(B), None);
        assert!(m.is_opening() && !cancel.load(Ordering::SeqCst), "the open runs on");
        m.open_progress(&video.key, 4, 9);
        assert_eq!(m.work(B), None, "progress has no row to show on");
        // The tour is listed again: the row shows the work again, where it is by now.
        reload(&mut m, three());
        assert_eq!((m.work(B), m.error(B)), (Some(Work::Opening(Some((4, 9)))), None));
        assert!(m.is_opening());
        // Still only one at a time.
        assert!(!m.begin_open(&key(&site().server, C, CH1), Arc::default()));
        m.errors.clear();
        // An open that fails for a tour whose row is gone says so above the list.
        reload(&mut m, vec![tour(A, "Customer onboarding", true, 22)]);
        m.open_ended(&video.key);
        m.edit_failed(&video.key, VIDEO_DAMAGED);
        assert_eq!(m.notice(), Some(&Notice { text: VIDEO_DAMAGED.into(), error: true }));
        assert!(!m.is_opening());

        // Another site's page shows none of it, and the open runs on there too.
        let mut m = loaded_with(three(), vec![video.clone()]);
        assert!(m.begin_open(&video.key, cancel.clone()));
        m.set_site(Some(Site { server: "https://other.example.com".into(), epoch: 1 }));
        assert_eq!(m.work(B), None);
        assert!(m.is_opening() && !cancel.load(Ordering::SeqCst));
        m.open_ended(&video.key);
        assert!(!m.is_opening());
        // An open asked for a tour the page does not list has no row, and is one all the same.
        let mut m = loaded(vec![tour(A, "Customer onboarding", true, 22)]);
        assert!(m.begin_open(&video.key, cancel.clone()));
        assert_eq!(m.work(B), None);
        assert!(m.is_opening());
    }

    #[test]
    fn a_video_project_reaches_the_app_as_a_loaded_project_once() {
        let (tmp, store) = temp_store();
        let saved = video_project(&store, &site().server, B, CH1, "Testing");
        let before = files(&saved.dir);
        let state = page_with(vec![saved.clone()]);
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(state.begin_open(&saved.key, cancel.clone()));
        let temp = new_edit_folder(&tmp.path().join("edits"));
        // What the row shows while each picture is read.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (model, shown) = (state.model.clone(), seen.clone());
        let codec = FakeCodec { during: Some(Box::new(move |_| shown.lock().unwrap().push(lock(&model).work(B)))), ..FakeCodec::default() };
        let open = VideoOpen { store: Some(&store), codec: Some(&codec), temp: &temp, cancel: &cancel, free: &any_room };
        let loaded = opened(&open, &saved.key, &state.open_rows()).unwrap_or_else(|e| panic!("the recording did not open: {e:?}"));
        assert_eq!((loaded.info.key.clone(), loaded.restored.title.as_str(), loaded.rec.frames.len()), (saved.key.clone(), "Testing", 3));
        assert_eq!(loaded.rec.frame(2).unwrap().as_raw()[..4], [2, 2, 2, 2], "picture 2 of the video is frame 2");
        // Every frame was reported to the row, in order.
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3);
        let done: Vec<u32> = seen.iter().map(|work| match work { Some(Work::Opening(Some((done, 3)))) => *done, other => panic!("the row showed {other:?}") }).collect();
        assert!(done.is_sorted() && done.iter().all(|d| *d <= 3), "{done:?}");
        // The frames are in the temporary folder, and the project is exactly as it was.
        assert_eq!(files(&temp).iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["frames.zst"]);
        assert!(files(&saved.dir) == before, "opening changed the project");
        assert_eq!(codec.decodes.load(Ordering::SeqCst), 1);
        // The app takes it: the row is an ordinary one again.
        state.open_ended(&saved.key);
        {
            let m = lock(&state.model);
            assert_eq!((m.work(B), m.is_opening()), (None, false));
        }
        // It is open like any project until its editor lets go, and the editor's delete takes the
        // temporary folder and never the video.
        assert!(matches!(store.load(&saved.key), Err(LoadError::AlreadyOpen)));
        let Loaded { rec, open: guard, .. } = loaded;
        rec.delete_files();
        drop(guard);
        assert!(!temp.exists());
        assert!(files(&saved.dir) == before);
    }

    #[test]
    fn a_cancelled_open_reaches_the_app_as_cancelled_and_leaves_no_temporary_folder() {
        let (tmp, store) = temp_store();
        let saved = video_project(&store, &site().server, B, CH1, "Testing");
        let before = files(&saved.dir);
        let state = page_with(vec![saved.clone()]);
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(state.begin_open(&saved.key, cancel.clone()));
        let temp = new_edit_folder(&tmp.path().join("edits"));
        // Cancel is pressed on the row while the second picture is read.
        let model = state.model.clone();
        let codec = FakeCodec {
            during: Some(Box::new(move |i| {
                if i == 1 {
                    lock(&model).cancel_open(B);
                }
            })),
            ..FakeCodec::default()
        };
        let open = VideoOpen { store: Some(&store), codec: Some(&codec), temp: &temp, cancel: &cancel, free: &any_room };
        let result = opened(&open, &saved.key, &state.open_rows());
        assert!(matches!(result, Err(LoadError::Cancelled)), "{:?}", result.err());
        assert!(!temp.exists(), "nothing is left of the folder");
        assert!(files(&saved.dir) == before, "the project is exactly as it was");
        // The app says nothing, and the row is free again.
        assert_eq!(load_sentence(&LoadError::Cancelled), None);
        state.open_ended(&saved.key);
        {
            let m = lock(&state.model);
            assert_eq!((m.work(B), m.error(B), m.is_opening()), (None, None, false));
        }
        // A cancel before the work began reads nothing at all.
        let before_start = FakeCodec::default();
        let open = VideoOpen { store: Some(&store), codec: Some(&before_start), temp: &temp, cancel: &cancel, free: &any_room };
        assert!(matches!(opened(&open, &saved.key, &state.open_rows()), Err(LoadError::Cancelled)));
        assert_eq!(before_start.decodes.load(Ordering::SeqCst), 0);
        assert!(!temp.exists());
        // The project was let go of: it opens the next time.
        let again = AtomicBool::new(false);
        let open = VideoOpen { store: Some(&store), codec: Some(&before_start), temp: &temp, cancel: &again, free: &any_room };
        let loaded = opened(&open, &saved.key, &state.open_rows()).expect("it opens after a cancel");
        loaded.rec.delete_files();
    }

    #[test]
    fn a_failed_open_puts_its_sentence_on_the_row() {
        let (tmp, store) = temp_store();
        let saved = video_project(&store, &site().server, B, CH1, "Testing");
        let before = files(&saved.dir);
        let state = page_with(vec![saved.clone()]);
        let cancel = AtomicBool::new(false);
        let temp = new_edit_folder(&tmp.path().join("edits"));
        let fine = FakeCodec::default();
        let unreadable = FakeCodec { fails: Some(|| CodecError::Failed { exit: Some(1) }), ..FakeCodec::default() };
        let gone = FakeCodec { fails: Some(|| CodecError::Failed { exit: None }), ..FakeCodec::default() };
        let breaks = FakeCodec { during: Some(Box::new(|_| panic!("the codec of this test breaks off"))), ..FakeCodec::default() };
        // The disk has one byte less than the frames took when they were stored as video.
        let needs = saved.frames_bytes;
        assert!(needs > 0);
        let asked = Mutex::new(Vec::new());
        let short = |folder: &Path| {
            asked.lock().unwrap().push(folder.to_path_buf());
            folder.is_dir().then_some(needs - 1)
        };
        // What is asked for, with which folder, codec and disk, and the sentence it ends in.
        type Case<'a> = (&'a str, Option<&'a Store>, Option<&'a dyn VideoCodec>, &'a (dyn Fn(&Path) -> Option<u64> + Sync), &'a str);
        let cases: [Case<'_>; 6] = [
            ("no ffmpeg", Some(&store), None, &any_room, NEEDS_FFMPEG),
            ("ffmpeg cannot be run", Some(&store), Some(&gone), &any_room, NEEDS_FFMPEG),
            ("the video cannot be read", Some(&store), Some(&unreadable), &any_room, VIDEO_DAMAGED),
            ("no room", Some(&store), Some(&fine), &short, NO_ROOM),
            ("no folder for saved recordings", None, Some(&fine), &any_room, NOT_SAVED),
            ("the work breaks off", Some(&store), Some(&breaks), &any_room, SAVED_DAMAGED),
        ];
        for (what, store, codec, free, sentence) in cases {
            assert!(state.begin_open(&saved.key, Arc::default()), "{what}");
            let open = VideoOpen { store, codec, temp: &temp, cancel: &cancel, free };
            let error = opened(&open, &saved.key, &state.open_rows()).err().unwrap_or_else(|| panic!("{what}: it opened"));
            assert!(!temp.exists(), "{what}: nothing is left of the folder");
            assert!(files(&saved.dir) == before, "{what}: the project is exactly as it was");
            // What the app does with it.
            state.open_ended(&saved.key);
            state.edit_failed(&saved.key, load_sentence(&error).unwrap_or_else(|| panic!("{what}: {error:?} has no sentence")));
            let m = lock(&state.model);
            assert_eq!((m.error(B), m.work(B)), (Some(sentence), None), "{what}: {error:?}");
            drop(m);
        }
        // Too little room is found before any work: the video was never read for it.
        assert_eq!(fine.decodes.load(Ordering::SeqCst), 0);
        // The room was asked of a folder that is there: the nearest one above the folder to be made.
        let answered = asked.lock().unwrap().last().cloned().expect("the room was asked about");
        assert!(answered.is_dir() && temp.starts_with(&answered) && answered.starts_with(tmp.path()), "{}", answered.display());
        // Exactly enough room, and a disk that does not say, are enough.
        let enough = |folder: &Path| folder.is_dir().then_some(needs);
        let open = VideoOpen { store: Some(&store), codec: Some(&fine), temp: &temp, cancel: &cancel, free: &enough };
        let loaded = opened(&open, &saved.key, &state.open_rows()).expect("it opens with exactly the room");
        loaded.rec.delete_files();
        drop(loaded);
        // None of it marked the project or left it open.
        assert_eq!(store.for_site(&site().server), vec![saved]);
    }

    #[test]
    fn a_frames_project_still_opens_without_a_thread() {
        let (_tmp, store) = temp_store();
        let server = site().server;
        let frames = save_project(&store, &server, B, CH1, "Testing");
        let video = video_project(&store, &server, A, CH1, "Getting started");
        let settings = crate::settings::Settings::default();
        // Only a recording kept as a video is opened on a thread.
        assert!(!crate::needs_thread(Some(&store), &frames.key));
        assert!(crate::needs_thread(Some(&store), &video.key));
        // Nothing saved, no folder, or a project file that cannot be read: answered at once, with the sentence.
        assert!(!crate::needs_thread(Some(&store), &key(&server, C, CH1)));
        assert!(!crate::needs_thread(None, &video.key));
        // The frames open as they always did, in one call on the caller's thread.
        let before = files(&frames.dir);
        let editor = crate::open_saved(Some(&store), &frames.key, &settings).expect("the saved recording opens in an editor");
        assert_eq!(crate::open_saved(Some(&store), &frames.key, &settings).err(), Some(ALREADY_OPEN));
        drop(editor);
        assert!(files(&frames.dir) == before);
        // A video asked for without the thread opens nothing and changes nothing.
        let before = files(&video.dir);
        assert_eq!(crate::open_saved(Some(&store), &video.key, &settings).err(), Some(NEEDS_FFMPEG));
        assert!(files(&video.dir) == before);
        std::fs::write(video.dir.join("project.json"), b"{ not a project").unwrap();
        assert!(!crate::needs_thread(Some(&store), &video.key));
        assert_eq!(crate::open_saved(Some(&store), &video.key, &settings).err(), Some(SAVED_DAMAGED));
    }

    /// Make the folder or file at `path` look last changed `ago` ago.
    fn aged(path: &Path, ago: Duration) {
        let mut options = std::fs::OpenOptions::new();
        options.write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_BACKUP_SEMANTICS: a folder is opened like a file.
            options.custom_flags(0x0200_0000);
        }
        #[cfg(not(windows))]
        if path.is_dir() {
            options.write(false).read(true);
        }
        options.open(path).unwrap().set_modified(std::time::SystemTime::now() - ago).unwrap();
    }

    #[test]
    fn only_stale_edit_folders_with_the_exact_name_and_nothing_but_frames_are_swept() {
        const OWN: u32 = 4242;
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("Mythicshot");
        std::fs::create_dir_all(&base).unwrap();
        let old = Duration::from_secs(2 * 60 * 60);
        // A folder with these files, all of it two hours old.
        let make = |name: &str, holds: &[&str]| {
            let dir = base.join(name);
            std::fs::create_dir(&dir).unwrap();
            for file in holds {
                std::fs::write(dir.join(file), b"frames").unwrap();
                aged(&dir.join(file), old);
            }
            aged(&dir, old);
            dir
        };
        // What an expansion that was cut off leaves: these go.
        let stale = make("edit-1-1700000000000000000", &["frames.zst"]);
        let empty = make("edit-77-5", &[]);
        // This run's own, whatever its age: it is an editor's recording.
        let own = make("edit-4242-1700000000000000000", &["frames.zst"]);
        // Less than an hour old, or holding a file that is: somebody may be at work there.
        let fresh = base.join("edit-1-2");
        std::fs::create_dir(&fresh).unwrap();
        std::fs::write(fresh.join("frames.zst"), b"frames").unwrap();
        let fresh_file = make("edit-1-3", &[]);
        std::fs::write(fresh_file.join("frames.zst"), b"frames").unwrap();
        aged(&fresh_file, old);
        // Anything else in it: not an expansion's folder as this version leaves one.
        let more = make("edit-1-4", &["frames.zst", "notes.txt"]);
        let other = make("edit-1-5", &["recording.json"]);
        let nested = make("edit-1-6", &[]);
        std::fs::create_dir(nested.join("frames.zst")).unwrap();
        aged(&nested, old);
        // Names that are not exactly the pattern.
        let names = ["edit-1", "edit-1-", "edit--1", "edit-1-2-3", "edit-1-x", "edit-x-1", "edit-1-7.bak", "xedit-1-8", "rec-1-9", "edit-+1-10", "edit_1_11", ".converting-1-12"];
        let odd: Vec<std::path::PathBuf> = names.iter().map(|name| make(name, &["frames.zst"])).collect();
        // A file with a folder's name.
        std::fs::write(base.join("edit-1-13"), b"a file").unwrap();
        aged(&base.join("edit-1-13"), old);
        // A link or a junction with the name, to a folder that looks like one of ours.
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("frames.zst"), b"somebody's frames").unwrap();
        aged(&outside.join("frames.zst"), old);
        aged(&outside, old);
        let link = base.join("edit-1-14");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&outside, &link).is_ok() || std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&outside).output().is_ok_and(|o| o.status.success());
        #[cfg(not(windows))]
        let linked = std::os::unix::fs::symlink(&outside, &link).is_ok();
        assert!(linked, "neither a folder link nor a junction could be made on this computer");

        sweep_stale_edits(&base, OWN, EDIT_STALE_AFTER);
        assert!(!stale.exists() && !empty.exists(), "what a cut-off expansion left is removed");
        assert_eq!(std::fs::read(own.join("frames.zst")).unwrap(), b"frames", "this run's own folder stays");
        assert!(fresh.join("frames.zst").is_file() && fresh_file.join("frames.zst").is_file(), "a folder somebody may be at work in stays");
        assert!(more.join("frames.zst").is_file() && more.join("notes.txt").is_file() && other.join("recording.json").is_file() && nested.join("frames.zst").is_dir(), "a folder that holds anything else stays whole");
        for dir in &odd {
            assert!(dir.join("frames.zst").is_file(), "{} is not an expansion's folder", dir.display());
        }
        assert!(base.join("edit-1-13").is_file());
        assert!(std::fs::symlink_metadata(&link).is_ok() && outside.join("frames.zst").is_file(), "a link is not followed and not removed");

        // With no age asked for, the fresh ones go too, and still nothing else.
        sweep_stale_edits(&base, OWN, Duration::ZERO);
        assert!(!fresh.exists() && !fresh_file.exists());
        assert!(own.join("frames.zst").is_file() && more.join("notes.txt").is_file() && other.join("recording.json").is_file() && nested.join("frames.zst").is_dir());
        assert!(odd.iter().all(|dir| dir.join("frames.zst").is_file()) && base.join("edit-1-13").is_file());
        assert!(std::fs::symlink_metadata(&link).is_ok() && outside.join("frames.zst").is_file());
        // Another run sees this one's as left behind, once it is old.
        sweep_stale_edits(&base, OWN + 1, EDIT_STALE_AFTER);
        assert!(!own.exists());
        // A folder that is not there, and a base that is a file, are nothing to sweep.
        sweep_stale_edits(&base.join("missing"), OWN, Duration::ZERO);
        sweep_stale_edits(&base.join("edit-1-13"), OWN, Duration::ZERO);
        assert!(base.join("edit-1-13").is_file());

        // The folder an open is given has exactly the name the sweep knows, with this process's number.
        let made = new_edit_folder(&base);
        let name = made.file_name().unwrap().to_str().unwrap();
        let (pid, nanos) = name.strip_prefix("edit-").unwrap().split_once('-').unwrap();
        assert_eq!(pid, std::process::id().to_string());
        assert!(!nanos.is_empty() && nanos.bytes().all(|b| b.is_ascii_digit()), "{name}");
        assert_eq!(made.parent(), Some(base.as_path()));
        assert_ne!(new_edit_folder(&base), new_edit_folder(&base), "two opens never share a folder");
        // Removing one's own folder takes the frames and nothing else.
        let mine = make("edit-9-1", &["frames.zst"]);
        remove_edit_folder(&mine);
        assert!(!mine.exists());
        remove_edit_folder(&more);
        assert!(more.join("notes.txt").is_file() && more.join("frames.zst").is_file(), "a folder that holds anything else is left whole");
        remove_edit_folder(&link);
        assert!(std::fs::symlink_metadata(&link).is_ok() && outside.join("frames.zst").is_file());
        remove_edit_folder(&base.join("missing"));
    }

    #[test]
    fn too_little_room_never_hides_the_truer_answer() {
        let (tmp, store) = temp_store();
        let server = site().server;
        let open_one = video_project(&store, &server, A, CH1, "Open in an editor");
        let cut_short = video_project(&store, &server, A, CH2, "Cut short");
        let damaged = video_project(&store, &server, B, CH1, "Damaged");
        let newer = video_project(&store, &server, B, CH2, "Newer");
        let sound = video_project(&store, &server, C, CH1, "Sound");
        let state = page_with(vec![]);
        let (cancel, codec) = (AtomicBool::new(false), FakeCodec::default());
        let temp = new_edit_folder(&tmp.path().join("edits"));
        // One is open in an editor; one lost half its video; one has a project file that cannot
        // be read; one was saved by a later version.
        let held = opened(&VideoOpen { store: Some(&store), codec: Some(&codec), temp: &new_edit_folder(&tmp.path().join("edits")), cancel: &cancel, free: &any_room }, &open_one.key, &state.open_rows()).expect("it opens");
        std::fs::write(cut_short.dir.join("frames.mkv"), b"half").unwrap();
        std::fs::write(damaged.dir.join("project.json"), b"{ not a project").unwrap();
        let mut later: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(newer.dir.join("project.json")).unwrap()).unwrap();
        later["format"] = serde_json::json!(project::FORMAT + 1);
        std::fs::write(newer.dir.join("project.json"), later.to_string()).unwrap();
        let before = files(store.root());
        let decodes = codec.decodes.load(Ordering::SeqCst);
        // The disk has no room at all, for any of them.
        let none = |folder: &Path| folder.is_dir().then_some(0u64);
        let open = VideoOpen { store: Some(&store), codec: Some(&codec), temp: &temp, cancel: &cancel, free: &none };
        let why = |key: &ProjectKey| opened(&open, key, &state.open_rows()).err().expect("nothing opens without room");
        assert_eq!(why(&open_one.key), LoadError::AlreadyOpen);
        assert!(matches!(why(&cut_short.key), LoadError::Damaged(_)));
        assert!(matches!(why(&damaged.key), LoadError::Damaged(_)));
        assert_eq!(why(&newer.key), LoadError::Newer);
        assert_eq!(why(&key(&server, C, CH2)), LoadError::NotSaved);
        // Only a recording with nothing else in its way is told about the room.
        assert_eq!(why(&sound.key), LoadError::NoRoom);
        // None of it read a video, made a folder, changed a project or left one open.
        assert_eq!(codec.decodes.load(Ordering::SeqCst), decodes);
        assert!(!temp.exists());
        assert!(files(store.root()) == before);
        let roomy = VideoOpen { store: Some(&store), codec: Some(&codec), temp: &temp, cancel: &cancel, free: &any_room };
        opened(&roomy, &sound.key, &state.open_rows()).expect("it opens once there is room").rec.delete_files();
        held.rec.delete_files();
    }

    #[test]
    fn a_row_that_lost_its_opening_state_has_it_back_when_the_tour_is_listed_again() {
        let video = video_info(B, CH1, "Testing", mb(32.2));
        let mut m = loaded_with(three(), vec![video.clone()]);
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(m.begin_open(&video.key, cancel.clone()));
        m.open_progress(&video.key, 3, 9);
        // One refresh misses the tour; the work goes on and reports.
        reload(&mut m, vec![tour(A, "Customer onboarding", true, 22)]);
        assert_eq!(m.work(B), None);
        m.open_progress(&video.key, 4, 9);
        // The next one lists it: the row shows where the work is, and has nothing else to press.
        reload(&mut m, three());
        assert_eq!(m.work(B), Some(Work::Opening(Some((4, 9)))));
        assert_eq!(m.ask_edit(B), None);
        m.ask_delete(B);
        m.start_rename(B);
        assert_eq!((m.edit(), m.error(B)), (&Edit::None, None));
        // The page belongs to another site for a while, with a tour of the same id: not its recording.
        let other = Site { server: "https://other.example.com".into(), epoch: 1 };
        m.set_site(Some(other));
        let (generation, _) = m.begin_load().unwrap();
        m.finish_load(generation, Ok(three()), Instant::now());
        assert_eq!(m.work(B), None);
        m.cancel_open(B);
        assert!(!cancel.load(Ordering::SeqCst), "another site's row stops nothing");
        m.open_progress(&video.key, 5, 9);
        // The site is set again: the row is back with its progress, and its Cancel works.
        m.set_site(Some(site()));
        let (generation, _) = m.begin_load().unwrap();
        m.finish_load(generation, Ok(three()), Instant::now());
        assert_eq!(m.work(B), Some(Work::Opening(Some((5, 9)))));
        m.cancel_open(B);
        assert!(cancel.load(Ordering::SeqCst));
        assert_eq!(m.work(B), None);
        // A cancelled open does not come back with the next list either.
        reload(&mut m, three());
        assert_eq!(m.work(B), None);
        m.open_ended(&video.key);
        assert!(!m.is_opening());
    }

    #[test]
    fn the_refusal_on_another_row_goes_when_the_open_has_ended() {
        let (first, second) = (video_info(B, CH1, "Testing", mb(32.2)), video_info(C, CH1, "Third", mb(8.0)));
        let mut m = loaded_with(three(), vec![first.clone(), second.clone()]);
        assert!(m.begin_open(&first.key, Arc::default()));
        assert_eq!(m.ask_edit(C), None);
        assert_eq!(m.error(C), Some(OPENING_ANOTHER));
        // Another row says something else: that stays.
        m.edit_failed(&key(&site().server, A, CH1), SAVED_DAMAGED);
        // The end of something else changes nothing.
        m.open_ended(&second.key);
        assert_eq!(m.error(C), Some(OPENING_ANOTHER));
        m.open_ended(&first.key);
        assert_eq!((m.error(C), m.error(A)), (None, Some(SAVED_DAMAGED)));
    }

    // ---- Publishing a tour whose restriction was not applied ----

    fn restricted_draft(id: &str) -> Vec<RemoteTour> {
        let mut tours = two();
        tours[1].id = id.to_string();
        tours
    }

    #[test]
    fn a_tour_whose_restriction_was_not_applied_asks_before_it_is_published_for_anyone() {
        let id = "tour_ffffffffffffffffffff0001";
        crate::publish_ui::note_unfinished_restriction(id);
        let mut m = loaded(restricted_draft(id));
        assert_eq!(m.request_publish(id), None, "nothing is sent yet");
        assert_eq!(m.edit(), &Edit::ConfirmPublish { id: id.into() });
        // Cancel, the default, sends nothing and keeps the tour remembered.
        m.cancel_edit();
        assert_eq!(m.edit(), &Edit::None);
        assert!(crate::publish_ui::restriction_unfinished(id));
        assert_eq!(confirm_publish_buttons(), [("Cancel", true), ("Publish for anyone", false)]);
        // Only the tour whose question shows can be published from it.
        assert_eq!(m.request_publish(id), None);
        assert_eq!(m.confirm_publish(B), None);
        let job = m.confirm_publish(id).expect("publish for anyone");
        assert_eq!((job.id.as_str(), &job.job), (id, &Job::Publish));
        assert_eq!(m.edit(), &Edit::None);
        assert!(!crate::publish_ui::restriction_unfinished(id), "the user chose anyone with the link");
        assert_eq!(m.confirm_publish(id), None, "a second click finds nothing to confirm");
    }

    #[test]
    fn a_tour_that_was_never_restricted_publishes_from_the_tours_page_as_before() {
        let mut m = loaded(two());
        let job = m.request_publish(B).expect("published at once");
        assert_eq!(job, Started { site: site(), id: B.into(), title: "Testing".into(), job: Job::Publish, listed: true });
        assert_eq!(m.edit(), &Edit::None);
    }

    /// One frame of the page with a key pressed.
    fn key_frame(ctx: &egui::Context, state: &mut ToursState, conn: &Conn, key: egui::Key) {
        let mut page_out = PageOut::default();
        let events = vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }];
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(590.0, 520.0))), events, ..Default::default() };
        ctx.run_ui(input, |ui| super::ui(ui, state, conn, &mut page_out)).textures_delta.clear();
    }

    /// The rectangle of the one piece of text equal to `text` one frame draws.
    fn text_rect(ctx: &egui::Context, state: &mut ToursState, conn: &Conn, text: &str) -> egui::Rect {
        fn collect(shape: &egui::Shape, text: &str, into: &mut Vec<egui::Rect>) {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == text => into.push(egui::Rect::from_min_size(t.pos, t.galley.size())),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, text, into)),
                _ => {}
            }
        }
        let mut page_out = PageOut::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(590.0, 520.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| super::ui(ui, state, conn, &mut page_out));
        out.textures_delta.clear();
        let mut found = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, text, &mut found));
        assert_eq!(found.len(), 1, "{text}");
        found[0]
    }

    #[test]
    fn the_question_before_publishing_a_remembered_tour_for_anyone_is_drawn_and_cancel_is_the_default() {
        const ID: &str = "tour_ffffffffffffffffffff0002";
        let list = format!(r#"{{"tours":[{{"id":"{ID}","title":"Restricted draft","status":"draft","slug":"x1","url":null,"chapters":1,"steps":3,"updated_at":1763424000000,"published_at":null}}]}}"#);
        let publish_line = format!("POST /api/v1/tours/{ID}/publish");
        let s = stub(move |req, _| if req.line == "GET /api/v1/tours" { reply(200, &list) } else { reply(200, r#"{"slug":"x1","url":"/t/x1","steps":3,"access":"public","viewers":0}"#) });
        crate::publish_ui::note_unfinished_restriction(ID);
        let (ctx, mut state, conn) = (crate::theme::test_context(), page(), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| !m.is_loading() && m.tours().len() == 1);
        let sent = || s.lines().iter().filter(|l| **l == publish_line).count();
        let asking = |state: &ToursState| lock(&state.model).edit() == &Edit::ConfirmPublish { id: ID.into() };
        let publish = places(&ctx, &mut state, &conn, "Publish")[0];
        // Publish: the question, and nothing sent.
        click(&ctx, &mut state, &conn, publish);
        assert!(asking(&state));
        let (text, _) = drawn(&ctx, &mut state, &conn);
        for line in [crate::share_ui::who::CONFIRM_HEADING, "Cancel", "Publish for anyone"] {
            assert!(has_line(&text, line), "{line:?} in:\n{text}");
        }
        assert!(text.replace('\n', " ").contains(crate::share_ui::who::CONFIRM_TEXT), "{text}");
        assert!(!has_line(&text, "Publish"), "the row's own buttons give way to the question:\n{text}");
        // The confirming button is never where the row's Publish was: a double click cannot reach it.
        let confirm = text_rect(&ctx, &mut state, &conn, "Publish for anyone").expand(12.0);
        assert!(!confirm.contains(publish), "{confirm:?} holds {publish:?}");
        // Enter and Escape choose Cancel.
        key_frame(&ctx, &mut state, &conn, egui::Key::Enter);
        assert!(!asking(&state));
        click(&ctx, &mut state, &conn, publish);
        key_frame(&ctx, &mut state, &conn, egui::Key::Escape);
        assert!(!asking(&state));
        // A click on Cancel closes it.
        click(&ctx, &mut state, &conn, publish);
        let cancel = places(&ctx, &mut state, &conn, "Cancel")[0];
        click(&ctx, &mut state, &conn, cancel);
        assert!(!asking(&state));
        // A second press where Publish was, while the question shows, publishes nothing.
        click(&ctx, &mut state, &conn, publish);
        click(&ctx, &mut state, &conn, publish);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sent(), 0, "nothing was published by any of that");
        assert!(crate::publish_ui::restriction_unfinished(ID));
        // Only "Publish for anyone" publishes.
        if !asking(&state) {
            click(&ctx, &mut state, &conn, publish);
        }
        let anyone = places(&ctx, &mut state, &conn, "Publish for anyone")[0];
        click(&ctx, &mut state, &conn, anyone);
        until(&ctx, &mut state, &conn, |_, _| sent() == 1);
        assert!(!crate::publish_ui::restriction_unfinished(ID));
    }
}
