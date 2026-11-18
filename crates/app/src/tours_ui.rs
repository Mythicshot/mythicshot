//! The Tours page of the main window: the tours on the connected site, and what can be done
//! with each: copy its link, open it, publish, unpublish, rename and delete.
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local, TimeZone};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, vec2};

use crate::main_window::{Page, PageOut};
use crate::publish::{self, Images, RemoteTour};
use crate::publish_ui::{self, TITLE_MAX, TokenStatus};
use crate::settings::UnfinishedDelete;

const MUTED: Color32 = Color32::from_rgb(163, 170, 184);
const ERROR: Color32 = Color32::from_rgb(255, 140, 140);
const PRIMARY: Color32 = Color32::from_rgb(47, 111, 235);
const CARD: Color32 = Color32::from_rgb(27, 31, 39);
const DANGER: Color32 = Color32::from_rgb(240, 110, 110);
const DANGER_TINT: Color32 = Color32::from_rgb(58, 26, 28);
const PUBLIC_TEXT: Color32 = Color32::from_rgb(125, 211, 140);
const PUBLIC_BG: Color32 = Color32::from_rgb(24, 52, 34);
const DRAFT_BG: Color32 = Color32::from_rgb(44, 50, 62);

pub const NOT_CONNECTED: &str = "Mythicshot is not connected to a tour site yet.";
pub const OPEN_WORKSPACE: &str = "Open Workspace";
pub const CHECKING: &str = "Checking for a saved token…";
pub const LOADING: &str = "Loading the tours…";
pub const EMPTY: &str = "This workspace has no tours yet. Record a tour and publish it to see it here.";
pub const RECORD_A_TOUR: &str = "Record a tour";
pub const RENAME_NOTE: &str = "The link stays the same after a rename.";
pub const LINK_COPIED: &str = "Link copied";
pub const LIST_CHANGED: &str = "The tour list changed, so nothing was deleted. Check the list and choose Delete again.";

pub const FINISH_DELETING: &str = "Finish deleting";
pub const JOB_BROKE: &str = "That did not finish because of an unexpected error in Mythicshot. The list is read again to show where the tour stands.";
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

/// The muted line under a title: "2 chapters · 22 steps · published 18 Nov", or for a draft
/// "… · updated 18 Nov" (the site's date for a draft moves with every change, a rename
/// included). A site that gives no date (an older one) gets the line without it.
pub fn detail_line<Tz: TimeZone>(t: &RemoteTour, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut line = format!("{} · {}", count(t.chapters.into(), "chapter", "chapters"), count(t.steps.into(), "step", "steps"));
    let dated = if t.is_public() { t.published_at.map(|ms| ("published", ms)) } else { t.updated_at.map(|ms| ("updated", ms)) };
    if let Some((word, day)) = dated.and_then(|(word, ms)| Some((word, day_label(ms, now)?))) {
        line.push_str(&format!(" · {word} {day}"));
    }
    line
}

/// A button on a tour's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowButton {
    CopyLink,
    Open,
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
            RowButton::Publish => "Publish",
            RowButton::Rename => "Rename",
            RowButton::Unpublish => "Unpublish",
            RowButton::Delete => "Delete",
        }
    }
}

/// The buttons a tour's row shows, in order. A public tour whose link the site did not give
/// (an older site) has no Copy link and no Open: there is nothing true to copy.
pub fn row_buttons(t: &RemoteTour) -> Vec<RowButton> {
    use RowButton::*;
    match (t.is_public(), t.link.is_some()) {
        (true, true) => vec![CopyLink, Open, Rename, Unpublish, Delete],
        (true, false) => vec![Rename, Unpublish, Delete],
        (false, _) => vec![Publish, Rename, Delete],
    }
}

/// The question the delete confirmation asks.
pub fn delete_question(t: &RemoteTour) -> String {
    format!("Delete \"{}\"? Its {} and images are removed from the site. This can't be undone.", publish_ui::short_title(&t.title, 60), count(t.steps.into(), "step", "steps"))
}

/// The line under the list: "2 tours in this workspace".
pub fn footer(tours: usize) -> String {
    format!("{} in this workspace", count(tours as u64, "tour", "tours"))
}

/// What the block for an unfinished delete says.
pub fn unfinished_sentence(title: &str) -> String {
    format!("Deleting \"{}\" did not finish. Its remaining files are still on the site.", publish_ui::short_title(title, 60))
}

/// What a job on a tour is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Publishing(Option<Images>),
    Unpublishing,
    Renaming,
    Deleting(Option<Images>),
}

/// "Publishing… 12 of 22 images", "Deleting… 250 of 601 files".
pub fn work_message(work: Work) -> String {
    match work {
        Work::Publishing(images) => publish_ui::publishing_message(images),
        Work::Unpublishing => "Unpublishing…".to_string(),
        Work::Renaming => "Saving the new name…".to_string(),
        Work::Deleting(None) => "Deleting…".to_string(),
        Work::Deleting(Some((done, 1))) => format!("Deleting… {done} of 1 file"),
        Work::Deleting(Some((done, total))) => format!("Deleting… {done} of {total} files"),
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// A job that worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    Published(Option<String>),
    Unpublished,
    Renamed(String),
    Deleted,
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
        self.working.get(id).copied()
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

    fn tour_mut(&mut self, id: &str) -> Option<&mut RemoteTour> {
        self.tours.iter_mut().find(|t| t.id == id)
    }

    /// Whether an action may start on this tour: it is in the list and nothing runs on it.
    fn can_act(&self, id: &str) -> bool {
        self.site.is_some() && self.tour(id).is_some() && !self.working.contains_key(id)
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
            Edit::Renaming { id, .. } => {
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
        Some(Started { site, id: id.to_string(), title: entry.title.clone(), job: Job::Delete })
    }

    fn begin(&mut self, id: &str, work: Work, job: Job) -> Option<Started> {
        let site = self.site.clone()?;
        // An edit of this tour ends when a job on it starts.
        if matches!(&self.edit, Edit::Renaming { id: e, .. } | Edit::ConfirmDelete { id: e } if e == id) {
            self.edit = Edit::None;
        }
        self.errors.remove(id);
        self.working.insert(id.to_string(), work);
        let title = self.tour(id).map(|t| t.title.clone()).unwrap_or_default();
        Some(Started { site, id: id.to_string(), title, job })
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
            if matches!(&m.edit, Edit::Renaming { id: e, .. } | Edit::ConfirmDelete { id: e } if e == id) {
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
                }
            }
            Ok(Done::Unpublished) => {
                if let Some(t) = self.tour_mut(id) {
                    t.status = "draft".to_string();
                    t.link = None;
                    t.published_at = None;
                }
            }
            Ok(Done::Deleted) => removed(self),
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

// ---- Deletes that did not finish ----

/// The deletes that removed a tour but not yet all of its files. One list, shared by the
/// page, its jobs and the app: a job writes into it the moment it knows, and the app
/// copies every change into the settings file, from where it is read back at start-up.
/// Entries of other sites than the configured one are kept, and shown when that site is
/// configured again.
#[derive(Clone, Default)]
pub struct Unfinished(Arc<Mutex<UnfinishedList>>);

#[derive(Default)]
struct UnfinishedList {
    list: Vec<UnfinishedDelete>,
    /// The settings file does not hold this list yet.
    changed: bool,
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

    /// Note that the delete of this tour on `server` (an address as the address rule writes it) is unfinished.
    pub fn add(&self, server: &str, tour_id: &str, title: &str) {
        let mut l = self.lock();
        if l.list.iter().any(|e| e.server == server && e.tour_id == tour_id) {
            return;
        }
        let title = if title.trim().is_empty() { "Untitled tour".to_string() } else { title.chars().take(TITLE_MAX).collect() };
        l.list.push(UnfinishedDelete { server: server.to_string(), tour_id: tour_id.to_string(), title });
        let extra = l.list.len().saturating_sub(MAX_UNFINISHED);
        l.list.drain(..extra);
        l.changed = true;
    }

    /// The site said nothing of this tour is left.
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
    pub fn take_changed(&self) -> Option<Vec<UnfinishedDelete>> {
        let mut l = self.lock();
        std::mem::take(&mut l.changed).then(|| l.list.clone())
    }

    pub fn is_changed(&self) -> bool {
        self.lock().changed
    }
}

// ---- Running it ----

/// Where a job gets the token for the site address it is about to use.
type TokenSource = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// The page's state: the model, shared with the jobs, the unfinished deletes, and where a job gets the token.
pub struct ToursState {
    model: Arc<Mutex<Model>>,
    token: TokenSource,
    unfinished: Unfinished,
}

impl Default for ToursState {
    fn default() -> Self {
        Self { model: Arc::default(), token: Arc::new(publish_ui::token_for), unfinished: Unfinished::default() }
    }
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

/// Do the job's requests. A delete is noted as unfinished from the moment the site has
/// begun it, and the note goes when the site says nothing is left (a tour that is already
/// gone included). Any other end leaves the note: files may still be there.
fn run_job(model: &Mutex<Model>, token: &TokenSource, unfinished: &Unfinished, started: &Started) -> Result<Done, Failure> {
    let (server, id) = (started.site.server.as_str(), started.id.as_str());
    let token = token(server).map_err(Failure::Message)?;
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
            let mut reports = 0u32;
            let result = publish::delete_tour(server, &token, id, |files| {
                // The second report: the site answered that the delete is under way.
                reports += 1;
                if reports == 2 {
                    unfinished.add(server, id, &started.title);
                }
                lock(model).progress(id, Work::Deleting(files));
            });
            match result {
                Ok(()) => {
                    unfinished.remove(server, id);
                    Ok(Done::Deleted)
                }
                Err(f) if f.unfinished => {
                    unfinished.add(server, id, &started.title);
                    Err(Failure::DeleteUnfinished(f.message))
                }
                Err(f) => Err(Failure::Message(f.message)),
            }
        }
    }
}

impl ToursState {
    /// A page whose jobs get the token from `token` instead of the credential store.
    #[cfg(test)]
    fn with_token(token: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Self {
        Self { model: Arc::default(), token: Arc::new(token), unfinished: Unfinished::default() }
    }

    /// The unfinished deletes, for the app to load at start-up and to write to the settings file.
    pub fn unfinished(&self) -> Unfinished {
        self.unfinished.clone()
    }

    fn spawn_load(&self, (generation, site): (u64, Site)) {
        let (model, token) = (self.model.clone(), self.token.clone());
        std::thread::spawn(move || {
            let mut guard = LoadGuard { model, generation, armed: true };
            let result = token(&site.server).and_then(|token| publish::list_tours(&site.server, &token));
            guard.armed = false;
            lock(&guard.model).finish_load(generation, result, Instant::now());
        });
    }

    fn spawn_job(&self, started: Started) {
        let (model, token, unfinished) = (self.model.clone(), self.token.clone(), self.unfinished.clone());
        std::thread::spawn(move || {
            let mut guard = JobGuard { model, started, armed: true };
            let result = run_job(&guard.model, &token, &unfinished, &guard.started);
            guard.armed = false;
            lock(&guard.model).finish(&guard.started, result);
        });
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
    DeleteTour(String),
    FinishDelete(UnfinishedDelete),
    DismissNotice,
}

/// Draw the page.
pub fn ui(ui: &mut egui::Ui, state: &mut ToursState, conn: &Conn, out: &mut PageOut) {
    let now = Instant::now();
    let mut m = lock(&state.model);
    m.set_site(conn.site());
    if m.wants_load(now)
        && let Some(load) = m.begin_load()
    {
        state.spawn_load(load);
    }
    let view = m.view(conn);
    let mut clicks = Vec::new();

    ui.horizontal(|ui| {
        ui.heading("Tours");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(egui::Button::new(RichText::new(RECORD_A_TOUR).strong()).fill(PRIMARY)).on_hover_text("Go to Home, where a recording starts").clicked() {
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
        for entry in state.unfinished.for_site(&site.server).into_iter().filter(|e| m.shows_unfinished(e)) {
            unfinished_ui(ui, &entry, m.work(&entry.tour_id), &mut clicks);
        }
    }

    match view {
        View::NotConnected(why) => {
            ui.add(egui::Label::new(RichText::new(why).color(MUTED)).wrap());
            ui.add_space(8.0);
            if ui.button(OPEN_WORKSPACE).clicked() {
                clicks.push(Click::Workspace);
            }
        }
        View::Checking => {
            ui.label(RichText::new(CHECKING).color(MUTED));
        }
        View::Loading => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new(LOADING).color(MUTED));
            });
        }
        View::Failed(e) => {
            ui.add(egui::Label::new(RichText::new(e).color(ERROR)).wrap());
            ui.add_space(8.0);
            if ui.add_enabled(!m.is_loading(), egui::Button::new("Retry")).clicked() {
                clicks.push(Click::Refresh);
            }
        }
        View::Empty => {
            ui.add(egui::Label::new(RichText::new(EMPTY).color(MUTED)).wrap());
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
                ui.label(RichText::new(footer_text).color(MUTED).size(12.0));
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
                RowButton::Rename => m.start_rename(&id),
                RowButton::Delete => m.ask_delete(&id),
                RowButton::Publish => {
                    if let Some(job) = m.start_publish(&id) {
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
    let waiting = m.is_loading() || !m.working.is_empty() || conn.token == TokenStatus::Checking || m.copied.is_some();
    ui.ctx().request_repaint_after(if waiting { Duration::from_millis(100) } else { STALE_AFTER });
}

fn notice_ui(ui: &mut egui::Ui, notice: &Notice, clicks: &mut Vec<Click>) {
    egui::Frame::new().fill(if notice.error { DANGER_TINT } else { CARD }).corner_radius(CornerRadius::same(8)).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(&notice.text).color(if notice.error { ERROR } else { MUTED })).wrap());
        if ui.button("Dismiss").clicked() {
            clicks.push(Click::DismissNotice);
        }
    });
    ui.add_space(8.0);
}

/// The block of one unfinished delete: what is left, and the button that carries on, or its progress.
fn unfinished_ui(ui: &mut egui::Ui, entry: &UnfinishedDelete, work: Option<Work>, clicks: &mut Vec<Click>) {
    egui::Frame::new().fill(DANGER_TINT).corner_radius(CornerRadius::same(8)).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(unfinished_sentence(&entry.title)).color(ERROR)).wrap());
        match work {
            Some(work) => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new(work_message(work)).color(MUTED));
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
    egui::Frame::new().fill(CARD).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
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
                    if (ui.add_enabled(can_save, egui::Button::new(RichText::new("Save").strong()).fill(PRIMARY)).clicked() || enter) && can_save {
                        clicks.push(Click::SaveRename);
                    }
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        clicks.push(Click::CancelEdit);
                    }
                });
                ui.label(RichText::new(RENAME_NOTE).color(MUTED).size(12.0));
            }
        } else {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    badge_ui(ui, t);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&t.title).size(15.0).strong()).truncate()).on_hover_text(&t.title);
                    });
                });
            });
        }
        ui.label(RichText::new(detail_line(t, today)).color(MUTED).size(12.0));
        ui.add_space(4.0);
        if let Some(work) = m.work(&t.id) {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new(work_message(work)).color(MUTED));
            });
        } else if confirming {
            egui::Frame::new().fill(DANGER_TINT).corner_radius(CornerRadius::same(6)).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(delete_question(t)).color(ERROR)).wrap());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep it").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        clicks.push(Click::CancelEdit);
                    }
                    if ui.add(egui::Button::new(RichText::new("Delete tour").color(Color32::WHITE)).fill(Color32::from_rgb(170, 48, 48))).clicked() {
                        clicks.push(Click::DeleteTour(t.id.clone()));
                    }
                });
            });
        } else if !renaming {
            ui.horizontal(|ui| {
                for b in row_buttons(t) {
                    let text = if b == RowButton::Delete { RichText::new(b.label()).color(DANGER) } else { RichText::new(b.label()) };
                    if ui.button(text).clicked() {
                        clicks.push(Click::Button(t.id.clone(), b, t.link.clone()));
                    }
                }
                if m.shows_copied(&t.id, now) {
                    ui.label(RichText::new(LINK_COPIED).color(MUTED));
                }
            });
        }
        if let Some(e) = m.error(&t.id) {
            ui.add(egui::Label::new(RichText::new(e).color(ERROR).size(12.0)).wrap());
        }
    });
    ui.add_space(6.0);
}

fn badge_ui(ui: &mut egui::Ui, t: &RemoteTour) {
    let (fg, bg) = if t.is_public() { (PUBLIC_TEXT, PUBLIC_BG) } else { (MUTED, DRAFT_BG) };
    egui::Frame::new().fill(bg).corner_radius(CornerRadius::same(9)).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
        ui.label(RichText::new(badge(t)).color(fg).size(11.0));
    });
}

/// The Refresh button: a circular arrow, drawn here.
fn refresh_button(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(6), DRAFT_BG);
    }
    let color = if enabled { Color32::from_rgb(214, 218, 226) } else { Color32::from_gray(100) };
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
mod tests {
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
        assert_eq!(detail_line(&t, &now), "2 chapters · 22 steps · published 18 Nov");
        let mut d = tour(B, "Testing", false, 9);
        d.chapters = 1;
        d.updated_at = Some(Utc.with_ymd_and_hms(2026, 11, 3, 8, 0, 0).unwrap().timestamp_millis());
        assert_eq!(detail_line(&d, &now), "1 chapter · 9 steps · updated 3 Nov");
        d.steps = 1;
        assert_eq!(detail_line(&d, &now), "1 chapter · 1 step · updated 3 Nov");
        // Another year is named.
        t.published_at = Some(Utc.with_ymd_and_hms(2025, 12, 31, 8, 0, 0).unwrap().timestamp_millis());
        assert_eq!(detail_line(&t, &now), "2 chapters · 22 steps · published 31 Dec 2025");
        // An older site gives no publish date: the line says nothing it does not know.
        t.published_at = None;
        assert_eq!(detail_line(&t, &now), "2 chapters · 22 steps");
        d.updated_at = None;
        assert_eq!(detail_line(&d, &now), "1 chapter · 1 step");
        // A time no calendar has.
        d.updated_at = Some(i64::MAX);
        assert_eq!(detail_line(&d, &now), "1 chapter · 1 step");
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
        assert_eq!(row_buttons(&public), [CopyLink, Open, Rename, Unpublish, Delete]);
        assert_eq!(row_buttons(&public).iter().map(|b| b.label()).collect::<Vec<_>>(), ["Copy link", "Open", "Rename", "Unpublish", "Delete"]);
        let draft = tour(B, "B", false, 3);
        assert_eq!(row_buttons(&draft), [Publish, Rename, Delete]);
        assert_eq!(row_buttons(&draft).iter().map(|b| b.label()).collect::<Vec<_>>(), ["Publish", "Rename", "Delete"]);
        assert_eq!((badge(&public), badge(&draft)), ("Public", "Draft"));
        // An older site names no link: nothing to copy or open, the rest stays.
        let mut unlinked = public.clone();
        unlinked.link = None;
        assert_eq!(row_buttons(&unlinked), [Rename, Unpublish, Delete]);
        // A draft never offers a link, whatever the row holds.
        let mut odd = draft.clone();
        odd.link = Some("https://tours.example.com/t/x".into());
        assert_eq!(row_buttons(&odd), [Publish, Rename, Delete]);
    }

    #[test]
    fn the_delete_question_names_the_tour_and_its_steps() {
        assert_eq!(delete_question(&tour(B, "Testing", false, 9)), "Delete \"Testing\"? Its 9 steps and images are removed from the site. This can't be undone.");
        assert_eq!(delete_question(&tour(B, "One", false, 1)), "Delete \"One\"? Its 1 step and images are removed from the site. This can't be undone.");
        let long = delete_question(&tour(B, &"x".repeat(300), false, 2));
        assert!(long.chars().count() < 160 && long.contains('…'), "{long}");
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
        assert_eq!(job, Started { site: site(), id: B.into(), title: "Testing".into(), job: Job::Rename("Trial run".into()) });
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
        assert_eq!(job, Started { site: site(), id: A.into(), title: "Customer onboarding".into(), job: Job::Delete });
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
        assert_eq!(again, Started { site: site(), id: B.into(), title: "Testing".into(), job: Job::Delete });
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

    #[test]
    fn the_unfinished_list_keeps_every_site_and_shows_one() {
        let u = Unfinished::default();
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
        let u = Unfinished::default();
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
        let u = Unfinished::default();
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
        assert_eq!(unfinished_sentence("Testing"), "Deleting \"Testing\" did not finish. Its remaining files are still on the site.");
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

    fn page() -> ToursState {
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
        let (ctx, mut state, conn) = (egui::Context::default(), page(), stub_conn(&s));
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
        let ctx = egui::Context::default();
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
        let (ctx, mut state, conn) = (egui::Context::default(), page(), stub_conn(&s));
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
        let (ctx, mut state, conn) = (egui::Context::default(), page(), stub_conn(&s));
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
        let (ctx, mut state, conn) = (egui::Context::default(), page(), stub_conn(&s));
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
        let (ctx, mut state, conn) = (egui::Context::default(), page(), stub_conn(&s));
        let store = state.unfinished();
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let job = {
            let mut m = lock(&state.model);
            m.ask_delete(B);
            m.confirm_delete(B).unwrap()
        };
        state.spawn_job(job);
        // It is written down while the job still runs: from the site's first "in progress".
        let deadline = Instant::now() + Duration::from_secs(20);
        while store.for_site(&s.base).is_empty() {
            assert!(Instant::now() < deadline, "the delete was never noted");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(lock(&state.model).work(B).is_some(), "noted before the loop gave up");
        let sentence = unfinished_sentence("Testing");
        let text = until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && !m.is_loading() && m.tours().len() == 1);
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
        let (ctx, mut state, conn) = (egui::Context::default(), page(), stub_conn(&s));
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

    #[test]
    fn a_delete_that_fails_before_the_site_began_is_not_written_down() {
        let s = stub(|req, _| if req.line == "GET /api/v1/tours" { reply(200, LIST) } else { reply(409, r#"{"error":"publish_busy"}"#) });
        let (ctx, mut state, conn) = (egui::Context::default(), page(), stub_conn(&s));
        until(&ctx, &mut state, &conn, |m, _| m.tours().len() == 2);
        let job = {
            let mut m = lock(&state.model);
            m.ask_delete(B);
            m.confirm_delete(B).unwrap()
        };
        state.spawn_job(job);
        until(&ctx, &mut state, &conn, |m, _| m.work(B).is_none() && m.error(B).is_some());
        assert!(state.unfinished().for_site(&s.base).is_empty() && !state.unfinished().is_changed());
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
        let (ctx, conn) = (egui::Context::default(), stub_conn(&s));
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
        let (ctx, mut state) = (egui::Context::default(), page());
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
        let ctx = egui::Context::default();
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
}
