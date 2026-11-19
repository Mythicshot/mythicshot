//! Where saved projects live, and how they are saved, listed, opened and removed.
//!
//! A project is the folder `<root>/<site>/<tour id>/<chapter id>/`: `project.json`, `recording.json`,
//! `frames.zst`, the cover logo and `pictures/`. `<site>` is the first 16 hex characters of the
//! SHA-256 of the site's address. A folder is a project only when it holds `project.json`, which is
//! written last when a project is saved and removed first when one is removed, and no `.removing`
//! mark, which a removal writes before anything else.
//!
//! A first save is built in `<root>/.staging-<time>-<random>/` and renamed into place. A project
//! that is open in an editor has its frames file open and cannot be renamed, so a save over it
//! writes the new pictures beside the old ones and then replaces `project.json` in one rename. A
//! project that is replaced whole waits in `<root>/.replaced-<random>/` until the new one is in
//! place; nothing but [`Store::recover_replaced`] ever takes such a folder away.
//!
//! Only files the store itself writes are ever removed: a file somebody else put into a project
//! folder stays where it is.
//!
//! A project keeps its recording as frames (`frames.zst`, `project.json` of format 1) or, once
//! a conversion verified it, as a video (`frames.mkv`, format 2). The video is made in
//! `<root>/.converting-<time>-<random>/` and swapped in by [`Store::swap_to_video`], whose
//! commit point is the one rename of `project.json`: before it the project is entirely its frames
//! form, after it entirely its video form, and the frames file goes last.
//!
//! At start-up, in this order: [`Store::recover_replaced`], [`Store::finish_removals`],
//! [`Store::finish_conversions`], [`Store::sweep_staging`], [`Store::sweep_converting`].

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

pub use mythicshot_recording::Form;
use mythicshot_recording::{CodecError, ExpandError, OpenError, Recording, SaveError, SaveStep, SavedAs, VIDEO_FILE, VideoCodec, expand_video, open_video_index};
use sha2::Digest;

use super::state::{self, Blob, ConvertMark, MAX_PROJECT_JSON_BYTES, ParseError, ProjectFile, ProjectKey, Restored, SavedForm, Snapshot, StoreInfo};

const PROJECT_FILE: &str = "project.json";
const INDEX_FILE: &str = "recording.json";
const FRAMES_FILE: &str = "frames.zst";
const PICTURES: &str = "pictures";
/// In a project folder that is being removed: written first, removed last.
const REMOVING_FILE: &str = ".removing";
/// A save in progress is `<root>/.staging-<milliseconds since 1970>-<16 hex>`.
const STAGING: &str = ".staging-";
/// A project moved out of a new save's way is `<root>/.replaced-<16 hex>`.
const REPLACED: &str = ".replaced-";
/// A conversion in progress is `<root>/.converting-<milliseconds since 1970>-<16 hex>`: the
/// video is made and verified there, and nothing but its own conversion ever writes into it.
const CONVERTING: &str = ".converting-";
/// How long a save over a project, or its removal, waits for a conversion of that project to
/// let go of its frames file.
const LET_GO: Duration = Duration::from_secs(2);
const RANDOM_CHARS: usize = 16;
/// The longest tour or chapter id that is used as a folder name.
const ID_MAX_CHARS: usize = 64;

/// How often a rename or a removal is tried while another program (a scanner, a sync) has the
/// file open: half a second in all.
const BUSY_TRIES: u32 = 5;
const BUSY_WAIT: Duration = Duration::from_millis(120);

/// The error of a save that could not put the project it was replacing back in place. The
/// wording is proposed and waits for the owner's approval.
pub const NOT_PUT_BACK: &str = "The earlier saved recording could not be put back in place. It is kept on this computer and is restored the next time Mythicshot starts.";

/// The reason of a save that was told to stop while the frames were being copied. The wording
/// is proposed and waits for the owner's approval.
pub const SAVING_CANCELLED: &str = "saving was cancelled.";
/// The reason of a save or a removal the disk had no room for. The wording is proposed and
/// waits for the owner's approval.
pub const DISK_FULL: &str = "the disk is full.";

/// The word a conversion mark carries when the video did not pass verification.
pub const MARK_FAILED: &str = "failed";
/// The word a conversion mark carries when the video was not enough smaller than the frames.
pub const MARK_NOT_SMALLER: &str = "not_smaller";

/// The Mythicshot version a conversion mark is made by, and the one it holds for.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The projects folder of this computer's user.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    /// [`epoch`]'s counter; a test gives each store its own.
    epoch: &'static AtomicU64,
    /// None in the app: the codec told to [`set_current_codec`]. A test gives a store its own.
    codec: Option<Option<String>>,
}

/// What the list knows about a project without opening its recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    /// The tour and chapter ids are the folder's names. The address is the one the project file
    /// names when it belongs in this folder, else the one asked for, else empty.
    pub key: ProjectKey,
    pub title: String,
    pub tour_title: String,
    /// Milliseconds since 1970.
    pub saved_at: i64,
    /// The size of the project's files together.
    pub bytes: u64,
    /// The size of the frames file alone, as the project file says: what the conversion queue
    /// measures a recording by. 0 when the project file cannot be read.
    pub frames_bytes: u64,
    pub dir: PathBuf,
    /// Why the project cannot be opened, as far as the cheap check can tell.
    pub problem: Option<Problem>,
    /// Whether the recording is kept as frames or as a video, as the project file says.
    pub form: Form,
    /// How storing the recording as video went, for the codec in use ([`set_current_codec`])
    /// and this version of Mythicshot.
    pub convert: ConvertState,
}

/// How the last try to store a project's recording as video ended. A try made with another
/// codec, or by another version of Mythicshot, is not one this version knows the end of: it
/// reads as [`ConvertState::NotTried`], and the project is tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertState {
    NotTried,
    /// The video did not pass verification, so the frames were kept.
    Failed,
    /// The video was not enough smaller than the frames, so the frames were kept.
    NotSmaller,
    /// The recording is kept as a video.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The project file cannot be read, or belongs to another tour or chapter.
    Damaged,
    /// The project was saved by a newer Mythicshot.
    Newer,
    /// The frames file or its index is not there, or the frames file is not as long as when it
    /// was saved. For a project kept as a video: the video or its index is not there, or the
    /// video is not as long as the project file says, or the project file names no size for it.
    FramesMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    /// Every project, also one with a problem: it takes its room on the disk all the same.
    pub count: usize,
    pub bytes: u64,
}

/// A project that was saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub info: ProjectInfo,
    /// What could not be saved with it, in words for a sentence: "the cover logo", "a pasted picture".
    pub left_out: Vec<String>,
}

/// A project opened for an editor. `open` keeps it registered as open until it is dropped; drop it
/// after everything that reads the recording.
pub struct Loaded {
    pub info: ProjectInfo,
    pub rec: Arc<Recording>,
    pub restored: Restored,
    pub open: OpenGuard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// There is no project for this address, tour and chapter.
    NotSaved,
    /// The project file or the recording cannot be used; the text says why.
    Damaged(String),
    /// Saved by a newer Mythicshot.
    Newer,
    /// The project is open in an editor.
    AlreadyOpen,
    /// The recording is kept as a video, and there is no ffmpeg to open it with: none was
    /// handed in, or the one found cannot be run. Nothing is wrong with the project.
    NeedsFfmpeg,
    /// The caller stopped the expansion of the video.
    Cancelled,
    /// The disk had no room for the expanded frames.
    NoRoom,
    /// The video could not be read back into the frames its index names. Nothing was changed:
    /// it may be the video, or the ffmpeg that read it.
    VideoDamaged,
}

/// How a video is expanded when a project that keeps one is opened.
pub struct Expand<'a> {
    /// What decodes the video; None when there is no ffmpeg.
    pub codec: Option<&'a dyn VideoCodec>,
    /// The folder the frames are expanded into: not there yet, or empty. The store makes it
    /// when it is not there, and leaves it empty when the project could not be opened; the
    /// recording that is handed back owns it.
    pub temp: &'a Path,
    /// Asked before every frame with the frames done and their number; a no stops the work.
    pub keep_going: &'a dyn Fn(usize, usize) -> bool,
}

/// A project whose recording can be stored as video, as [`Store::next_candidate`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub key: ProjectKey,
    pub dir: PathBuf,
    /// When the project was saved, and the size of its frames file: what a swap or a mark
    /// checks again, so that neither is made on a project that was saved meanwhile.
    pub saved_at: i64,
    pub frames_bytes: u64,
}

/// What [`Store::look`] found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Look {
    /// The next project to store as video.
    pub next: Option<Candidate>,
    /// The projects that are kept from being the next by nothing but an editor that has them
    /// open. Closing an editor tells nobody, so whoever waits for these has to look again.
    pub open: Vec<ProjectKey>,
}

/// The one conversion that runs, from [`Store::begin_conversion`] until it is dropped: dropping
/// it lets the project go again and removes the conversion's folder with whatever is left in it.
#[derive(Debug)]
pub struct Converting {
    dir: PathBuf,
    work: PathBuf,
    cancel: Arc<AtomicBool>,
    epoch: &'static AtomicU64,
}

/// What [`Store::remove_project`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removed {
    /// The project's files are gone.
    Now,
    /// The project is open in an editor: it is no longer listed, and its files go when that
    /// editor closes it, or at the next start.
    WhenItsEditorCloses,
    /// There was no project in that folder, and nothing was touched or only a removal that was
    /// already under way was finished.
    NotThere,
}

/// Registers a project as open until dropped. A project whose tour was removed while it was open
/// is removed when its guard is dropped.
#[derive(Debug)]
pub struct OpenGuard {
    dir: PathBuf,
    name: PathBuf,
    epoch: &'static AtomicU64,
}

/// The steps of a save that a test makes fail.
struct Steps<'a> {
    /// Puts the recording's frames file, or its video, and the index into a folder; what was
    /// kept, and its size.
    keep_frames: &'a dyn Fn(&Recording, &Path) -> Result<SavedAs, SaveError>,
    /// Every rename of a save: a staging folder into place, the project it replaces out of the
    /// way and back, and the new `project.json` over the old one. And the three of a swap: the
    /// video into the project, the new `project.json` over the old one, the video's index.
    rename: &'a dyn Fn(&Path, &Path) -> io::Result<()>,
}

/// A counter bumped by every save and every removal in this process, so a page knows when to
/// read the list again.
pub fn epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

static EPOCH: AtomicU64 = AtomicU64::new(0);

/// Tell every store of this process which codec stores recordings as video now (its
/// [`VideoCodec::id`](mythicshot_recording::VideoCodec::id)), or that there is none. A
/// [`ProjectInfo::convert`] is told for that codec: until one is named, and for a mark another
/// codec left, every project that is kept as frames reads as not tried.
pub fn set_current_codec(id: Option<&str>) {
    *current_codec() = id.map(str::to_string);
    // What a page shows of a recording was read for the codec before: it reads again.
    EPOCH.fetch_add(1, Ordering::SeqCst);
}

fn current_codec() -> MutexGuard<'static, Option<String>> {
    static CODEC: Mutex<Option<String>> = Mutex::new(None);
    CODEC.lock().unwrap_or_else(|e| e.into_inner())
}

impl Store {
    pub fn at(root: PathBuf) -> Store {
        Store { root, epoch: &EPOCH, codec: None }
    }

    /// The codec a mark is read for: the one in use, or none.
    fn codec_id(&self) -> Option<String> {
        match &self.codec {
            Some(own) => own.clone(),
            None => current_codec().clone(),
        }
    }

    /// Where projects live for this user: beside the app's local data, which does not roam. A
    /// debug build with `MYTHICSHOT_CONFIG_DIR` set uses that folder, as the settings file does.
    /// None when the user's folder cannot be found; there is no other place to fall back to.
    pub fn default_root() -> Option<PathBuf> {
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("MYTHICSHOT_CONFIG_DIR") {
            return Some(PathBuf::from(dir).join("projects"));
        }
        Self::root_in(directories::ProjectDirs::from("", "", "Mythicshot").map(|d| d.data_local_dir().to_path_buf()))
    }

    /// `projects` in the app's local data folder, and only in a folder with a full path.
    fn root_in(local_data: Option<PathBuf>) -> Option<PathBuf> {
        local_data.filter(|dir| dir.is_absolute()).map(|dir| dir.join("projects"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Save the project for `key`. The error is the reason in words that follow "…: " in a
    /// sentence for the user (or [`NOT_PUT_BACK`], a sentence of its own): lower-case, with one
    /// full stop, and without a folder's name or the system's own text.
    // Only the tests save this way: a publish saves with `save_while`, so that it can be cancelled.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn save(&self, key: &ProjectKey, snap: &Snapshot<'_>) -> Result<Saved, String> {
        self.save_while(key, snap, &|| true)
    }

    /// [`Store::save`] for a job that can be cancelled: when the frames have to be copied
    /// (they are linked when that can be done, which takes no time), `keep_going` is asked
    /// before every megabyte, and a no ends the save with [`SAVING_CANCELLED`] and nothing
    /// left behind. Nothing else of a save is cut short by it.
    pub fn save_while(&self, key: &ProjectKey, snap: &Snapshot<'_>, keep_going: &dyn Fn() -> bool) -> Result<Saved, String> {
        self.save_with(key, snap, &Steps { keep_frames: &|rec, dir| rec.save_into_with(dir, keep_going), rename: &patient_rename })
    }

    fn save_with(&self, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<Saved, String> {
        let dir = self.project_dir(key).ok_or("this tour's id cannot be used as a folder name.")?;
        let open_here = {
            let _changing = changing();
            is_project(&dir) && is_open(&dir)
        };
        let in_place = if open_here { save_in_place(&dir, key, snap, steps)? } else { None };
        let (file, left_out) = match in_place {
            Some(saved) => saved,
            None => self.save_whole(&dir, key, snap, steps)?,
        };
        self.bump();
        // Built from what was written: the save is done, whatever a second look at the folder says.
        let (form, convert) = (form_of_store(&file.store), convert_state(&file.store, self.codec_id().as_deref()));
        let info = ProjectInfo { key: key.clone(), title: file.title, tour_title: file.tour_title, saved_at: file.saved_at, bytes: bytes_in(&dir), frames_bytes: file.frames_bytes, dir, problem: None, form, convert };
        Ok(Saved { info, left_out })
    }

    /// Build the whole project in a staging folder and rename it into place. A project already
    /// there, which no editor has open, is moved out of the way first and put back if the new one
    /// cannot take its place.
    fn save_whole(&self, dir: &Path, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<(ProjectFile, Vec<String>), String> {
        if matches!(read_project(dir), Err(Unreadable::Newer)) {
            return Err("a newer version of Mythicshot saved this tour's recording, so it is left as it is.".into());
        }
        // The project that is there is about to be moved aside: nothing may hold its frames open.
        cancel_conversion_of(dir, LET_GO);
        std::fs::create_dir_all(&self.root).map_err(|e| reason("the folder for saved recordings could not be made", &e))?;
        let staging = self.root.join(format!("{STAGING}{}-{}", now_ms().max(0), random_name()));
        std::fs::create_dir(&staging).map_err(|e| reason("the folder for saved recordings could not be written", &e))?;
        // The frames may have to be copied, which can take long: nothing else waits for that.
        let result = fill(&staging, key, snap, steps).and_then(|saved| {
            let _changing = changing();
            self.place(&staging, dir, steps).map(|()| saved)
        });
        if result.is_err() {
            // Entirely this call's own: nothing else was ever in it.
            let _ = std::fs::remove_dir_all(&staging);
        }
        result
    }

    /// Rename the finished staging folder to `dir`, with no other change to the store under way.
    fn place(&self, staging: &Path, dir: &Path, steps: &Steps<'_>) -> Result<(), String> {
        // Opened while the staging folder was filled: its editor reads the frames that are there.
        if is_open(dir) {
            return Err("this tour's saved recording is open in an editor and could not be replaced.".into());
        }
        // A conversion that started on it while the staging folder was filled is told to stop.
        // Nothing waits for it here: this runs with the store's lock held.
        cancel_conversion_of(dir, Duration::ZERO);
        let parents: Vec<&Path> = dir.ancestors().skip(1).take(2).collect();
        if let Some(parent) = parents.first() {
            std::fs::create_dir_all(parent).map_err(|e| reason("the folder for saved recordings could not be written", &e))?;
        }
        let aside = match std::fs::symlink_metadata(dir) {
            Ok(_) => {
                let aside = self.root.join(format!("{REPLACED}{}", random_name()));
                (steps.rename)(dir, &aside).map_err(|e| reason("this tour's saved recording is in use and could not be replaced", &e))?;
                Some(aside)
            }
            Err(_) => None,
        };
        if let Err(e) = (steps.rename)(staging, dir) {
            match &aside {
                // Kept where it is under its own name; the next start puts it back.
                Some(aside) if (steps.rename)(aside, dir).is_err() => return Err(NOT_PUT_BACK.to_string()),
                Some(_) => {}
                // The folders made for it, when nothing else is in them.
                None => parents.iter().for_each(|parent| {
                    let _ = std::fs::remove_dir(parent);
                }),
            }
            return Err(reason("the recording could not be put into the folder for saved recordings", &e));
        }
        if let Some(aside) = &aside {
            retire(aside, dir);
        }
        Ok(())
    }

    /// Finish what a save that replaced a project could not: run at start-up, before
    /// [`Store::finish_removals`] and [`Store::sweep_staging`]. A `.replaced-` folder whose
    /// project file names a tour and chapter with no project in place is put back there. When a
    /// project is in place that passes the list's check and was saved no earlier, the files the
    /// store wrote go from the `.replaced-` folder, and the folder too once it is empty; with any
    /// other project in place the folder is left alone, and so is one without a readable project
    /// file. A folder whose clearing was cut off (it carries the removal mark) is cleared.
    pub fn recover_replaced(&self) {
        let _changing = changing();
        for (name, aside) in folders_in(&self.root) {
            if !name.strip_prefix(REPLACED).is_some_and(is_random_name) {
                continue;
            }
            if is_removing(&aside) {
                let _ = remove_files(&aside);
                continue;
            }
            let Some((key, saved_at)) = named_key(&aside) else { continue };
            let Some(dir) = self.project_dir(&key) else { continue };
            if is_project(&dir) {
                // Only a project that opens, and is no older, makes this copy one to clear away.
                let in_place = self.info(&dir, &site_of(&key.server), &key.tour_id, &key.chapter_id, Some(&key.server));
                if in_place.is_some_and(|p| p.problem.is_none() && p.saved_at >= saved_at) {
                    retire(&aside, &dir);
                }
                continue;
            }
            if let Some(parent) = dir.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // An empty folder in its place is no reason to keep it out.
            let _ = std::fs::remove_dir(&dir);
            if patiently(|| std::fs::rename(&aside, &dir)).is_ok() {
                self.bump();
            }
        }
    }

    /// Finish the removals a crash cut off, and those of projects that were open in an editor
    /// when the app last closed: run at start-up, after [`Store::recover_replaced`]. Every
    /// project folder that carries the removal mark, and is not open now, is removed.
    pub fn finish_removals(&self) {
        let _changing = changing();
        for (dir, _, _, _) in self.chapter_folders() {
            if is_removing(&dir) && !is_open(&dir) {
                let _ = remove_files(&dir);
                prune_parents(&dir);
                self.bump();
            }
        }
    }

    /// Every folder `<site>/<tour>/<chapter>` with names the store gives, with those names.
    fn chapter_folders(&self) -> Vec<(PathBuf, String, String, String)> {
        let mut out = Vec::new();
        for (site, site_dir) in folders_in(&self.root).into_iter().filter(|(site, _)| is_random_name(site)) {
            chapters_of_site(&site_dir, &site, &mut out);
        }
        out
    }

    /// Every project, of every address.
    pub fn list(&self) -> Vec<ProjectInfo> {
        let codec = self.codec_id();
        sorted(self.chapter_folders().into_iter().filter_map(|(dir, site, tour, chapter)| read_info(&dir, &site, &tour, &chapter, None, codec.as_deref())).collect())
    }

    /// Every project of one site, read in one walk. `server` is matched exactly.
    pub fn for_site(&self, server: &str) -> Vec<ProjectInfo> {
        let site = site_of(server);
        let mut folders = Vec::new();
        chapters_of_site(&self.root.join(&site), &site, &mut folders);
        let codec = self.codec_id();
        sorted(folders.into_iter().filter_map(|(dir, site, tour, chapter)| read_info(&dir, &site, &tour, &chapter, Some(server), codec.as_deref())).collect())
    }

    /// The projects of one tour on one site: one per saved chapter. `server` is matched exactly.
    // Only the tests ask for one tour: the Tours page reads a whole site at once with `for_site`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn for_tour(&self, server: &str, tour_id: &str) -> Vec<ProjectInfo> {
        if !is_folder_id(tour_id) {
            return Vec::new();
        }
        let (site, codec) = (site_of(server), self.codec_id());
        sorted(folders_in(&self.root.join(&site).join(tour_id)).into_iter().filter(|(chapter, _)| is_folder_id(chapter)).filter_map(|(chapter, dir)| read_info(&dir, &site, tour_id, &chapter, Some(server), codec.as_deref())).collect())
    }

    pub fn totals(&self) -> Totals {
        let list = self.list();
        Totals { count: list.len(), bytes: list.iter().fold(0, |sum, p| sum.saturating_add(p.bytes)) }
    }

    /// Open a project for an editor. Nothing is changed or removed, whatever is found. A
    /// project that keeps its recording as a video is not opened this way: it answers as
    /// [`Store::load_with`] does without a codec.
    pub fn load(&self, key: &ProjectKey) -> Result<Loaded, LoadError> {
        self.load_with(key, &Expand { codec: None, temp: Path::new(""), keep_going: &|_, _| true })
    }

    /// [`Store::load`], and a project that keeps its recording as a video is expanded into
    /// `expand.temp` and opened as the ordinary recording that makes: one that owns that folder
    /// and remembers the video it came from. This can take minutes; the project is registered as
    /// open for all of it, and nothing else of the store waits for it.
    ///
    /// Nothing in the project is changed, removed or marked, whatever is found and however the
    /// expansion ends. When it does not end with a recording, the temporary folder is left empty.
    /// A conversion of this project that is running is told to stop.
    pub fn load_with(&self, key: &ProjectKey, expand: &Expand<'_>) -> Result<Loaded, LoadError> {
        let dir = self.project_dir(key).ok_or(LoadError::NotSaved)?;
        // Registered first, so nothing replaces or removes the folder while it is read; a load
        // that fails drops the guard again.
        let open = {
            let _changing = changing();
            if !is_project(&dir) {
                return Err(LoadError::NotSaved);
            }
            if is_open(&dir) {
                return Err(LoadError::AlreadyOpen);
            }
            if is_removing(&dir) {
                return Err(LoadError::NotSaved);
            }
            let open = OpenGuard::register(&dir, self.epoch).ok_or(LoadError::AlreadyOpen)?;
            // An open project is never swapped; its conversion need not run on.
            cancel_conversion_of(&dir, Duration::ZERO);
            open
        };
        let file = read_project(&dir).map_err(|e| match e {
            Unreadable::Newer => LoadError::Newer,
            Unreadable::NotThere => LoadError::NotSaved,
            Unreadable::Damaged(why) => LoadError::Damaged(why),
        })?;
        if (file.server.as_str(), file.tour_id.as_str(), file.chapter_id.as_str()) != (key.server.as_str(), key.tour_id.as_str(), key.chapter_id.as_str()) {
            return Err(LoadError::Damaged("the project file belongs to another tour or chapter".into()));
        }
        // Pictures are read from the project's own folder and from nowhere a link points to.
        let pictures_ok = || if std::fs::symlink_metadata(dir.join(PICTURES)).is_ok() && !is_real_folder(&dir.join(PICTURES)) { Err(LoadError::Damaged("the project's pictures folder is not a folder".into())) } else { Ok(()) };
        let info = || self.info(&dir, &site_of(&key.server), &key.tour_id, &key.chapter_id, Some(&key.server)).ok_or(LoadError::NotSaved);
        let (info, rec) = match file.store.form {
            SavedForm::Frames => {
                let rec = Recording::open_saved(&dir).map_err(|e| match e {
                    OpenError::Missing => LoadError::Damaged("the recording's frames are not there".into()),
                    OpenError::Damaged(why) => LoadError::Damaged(why),
                    OpenError::Newer(_) => LoadError::Newer,
                    OpenError::Video => LoadError::Damaged("the recording's index describes a video, and the project keeps frames".into()),
                })?;
                if !frames_are(&dir, file.frames_bytes) {
                    return Err(LoadError::Damaged("the frames file is not the one the project was saved with".into()));
                }
                pictures_ok()?;
                (info()?, rec)
            }
            SavedForm::Video => {
                // Everything that can be refused is, before the long work starts.
                if !video_is(&dir, file.store.video_bytes) {
                    return Err(LoadError::Damaged("the video is not the one the project was saved with".into()));
                }
                let index = open_video_index(&dir).map_err(|e| match e {
                    OpenError::Missing => LoadError::Damaged("the recording's index is not there".into()),
                    OpenError::Damaged(why) => LoadError::Damaged(why),
                    OpenError::Newer(_) => LoadError::Newer,
                    OpenError::Video => LoadError::Damaged("the recording's index cannot be read".into()),
                })?;
                // An index of the frames form names no video: it is what a swap that was cut off leaves.
                if index.video_bytes.is_some_and(|noted| noted != file.store.video_bytes) {
                    return Err(LoadError::Damaged("the recording's index was not written for this video".into()));
                }
                pictures_ok()?;
                let info = info()?;
                let codec = expand.codec.ok_or_else(|| not_expanded(NotExpanded::NeedsFfmpeg))?;
                std::fs::create_dir_all(expand.temp).map_err(|e| expand_failed(ExpandError::Io(e)))?;
                let rec = expand_video(&index, &dir.join(VIDEO_FILE), expand.temp, codec, expand.keep_going).map_err(expand_failed)?;
                (info, rec)
            }
        };
        let restored = state::from_file(file, &dir, &rec);
        Ok(Loaded { info, rec: Arc::new(rec), restored, open })
    }

    /// Which form the project of `key` keeps its recording in, by its project file alone, so
    /// that a caller knows whether opening it can take long. None when there is no project
    /// there that this version can read, or it is being removed.
    pub fn form_of(&self, key: &ProjectKey) -> Option<Form> {
        let dir = self.project_dir(key)?;
        if is_removing(&dir) {
            return None;
        }
        read_project(&dir).ok().map(|file| form_of_store(&file.store))
    }

    /// The next project to store as video: the one saved longest ago among those that keep
    /// their frames, pass the list's check, open, are not open in an editor or being removed,
    /// have a frames file of at least `min_bytes`, and carry no mark of a try with the codec
    /// `codec_id` by this version of Mythicshot. Nothing is changed.
    // Only the tests ask this way: the queue asks `look`, which skips what it has given up.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn next_candidate(&self, codec_id: &str, min_bytes: u64) -> Option<Candidate> {
        self.look(codec_id, min_bytes, &|_, _| false).next
    }

    /// [`Store::next_candidate`] among the projects `skip` does not answer true for; it is
    /// asked with a project's key and its time of saving. For a queue that has given some
    /// projects up and goes on with the next oldest. The projects that would have been looked
    /// at too if no editor had them open are named beside the answer. Nothing is changed.
    pub fn look(&self, codec_id: &str, min_bytes: u64, skip: &dyn Fn(&ProjectKey, i64) -> bool) -> Look {
        let (mut found, mut open) = (Vec::new(), Vec::new());
        for (dir, site, tour, chapter) in self.chapter_folders() {
            if is_removing(&dir) {
                continue;
            }
            let Ok(file) = read_project(&dir) else { continue };
            let belongs = site_of(&file.server) == site && file.tour_id == tour && file.chapter_id == chapter;
            if belongs && file.store.form == SavedForm::Frames && file.frames_bytes >= min_bytes && payload_ok(&dir, &file) && mark_holds(&file.store, codec_id).is_none() {
                let key = ProjectKey { server: file.server.clone(), tour_id: file.tour_id.clone(), chapter_id: file.chapter_id.clone() };
                if skip(&key, file.saved_at) {
                    continue;
                }
                if is_open(&dir) {
                    open.push(key);
                } else {
                    found.push((file.saved_at, dir, key, file.frames_bytes));
                }
            }
        }
        found.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        // One whose index does not describe its frames file cannot be converted, or opened.
        let next = found.into_iter().find(|(_, dir, _, _)| Recording::open_saved(dir).is_ok()).map(|(saved_at, dir, key, frames_bytes)| Candidate { key, dir, saved_at, frames_bytes });
        Look { next, open }
    }

    /// Start the one conversion: None when another runs, or the project is no longer what the
    /// candidate describes (it was saved again, opened, or is being removed). The conversion's
    /// folder `<root>/.converting-<time>-<random>/` is made, and the epoch is bumped.
    pub fn begin_conversion(&self, c: &Candidate) -> Option<Converting> {
        let _changing = changing();
        self.candidate_file(c)?;
        let mut slot = converting();
        if slot.is_some() {
            return None;
        }
        let work = self.root.join(format!("{CONVERTING}{}-{}", now_ms().max(0), random_name()));
        std::fs::create_dir(&work).ok()?;
        let cancel = Arc::new(AtomicBool::new(false));
        *slot = Some(Slot { name: registry_name(&c.dir), dir: c.dir.clone(), work: work.clone(), cancel: cancel.clone() });
        drop(slot);
        self.bump();
        Some(Converting { dir: c.dir.clone(), work, cancel, epoch: self.epoch })
    }

    /// The project file of a candidate that is still what the candidate describes: a project
    /// of this store in its folder, kept as frames, not open, not being removed, saved when
    /// the candidate says, with a frames file that is the size the candidate says, on the
    /// disk too. Asked with no other change to the store under way.
    fn candidate_file(&self, c: &Candidate) -> Option<ProjectFile> {
        let dir = &c.dir;
        if self.project_dir(&c.key).as_ref() != Some(dir) || !is_project(dir) || is_open(dir) || is_removing(dir) {
            return None;
        }
        let file = read_project(dir).ok()?;
        let same = (file.server.as_str(), file.tour_id.as_str(), file.chapter_id.as_str()) == (c.key.server.as_str(), c.key.tour_id.as_str(), c.key.chapter_id.as_str())
            && file.store.form == SavedForm::Frames
            && (file.saved_at, file.frames_bytes) == (c.saved_at, c.frames_bytes)
            && frames_are(dir, c.frames_bytes)
            && has_index(dir);
        same.then_some(file)
    }

    /// Replace the frames of a candidate by the video its conversion made and verified, which
    /// lies in the conversion's folder and is `video_bytes` long. `Ok(false)`, and nothing
    /// touched, when the conversion is not this project's or was told to stop, the project is
    /// open, being removed or no longer what the candidate describes (its time of saving, the
    /// size its file names for the frames, the frames file's own size), or the video is not a
    /// file of `video_bytes`.
    ///
    /// With no other change to the store under way, in this order: the video's index is
    /// written beside the video; the video, on the disk, is renamed into the project; the
    /// project file that names it (format 2) replaces the old one in one rename; the index
    /// replaces the frames' index; the frames file is removed, under this project's name only.
    /// An error before the project file is replaced leaves the project as it was, in its frames
    /// form; a video that is no longer the verified file in the project when the project file
    /// is about to name it is such an error. After the commit the same look is taken before
    /// the index is replaced and before the frames are removed, and a video that changed
    /// keeps both. From that rename on the project is in its video form and the answer is `Ok(true)`:
    /// what could not be finished ([`Store::finish_conversions`] does it at the next start) is
    /// the frames' index or the frames file still lying there, which a video project ignores.
    /// The frames file is removed only after the new project file is on the disk.
    ///
    /// The error is a reason for a log line, as [`Store::save`]'s is.
    pub fn swap_to_video(&self, c: &Candidate, conv: &Converting, video_bytes: u64) -> Result<bool, String> {
        self.swap_with(c, conv, video_bytes, &Steps { keep_frames: &|rec, dir| rec.save_into_with(dir, &|| true), rename: &patient_rename })
    }

    fn swap_with(&self, c: &Candidate, conv: &Converting, video_bytes: u64, steps: &Steps<'_>) -> Result<bool, String> {
        const NOT_STORED: &str = "the recording could not be stored as video";
        let dir = &c.dir;
        let _changing = changing();
        if conv.dir != *dir || conv.cancelled() {
            return Ok(false);
        }
        let Some(mut file) = self.candidate_file(c) else { return Ok(false) };
        let made = conv.video_path();
        if video_bytes == 0 || !std::fs::symlink_metadata(&made).is_ok_and(|m| m.is_file() && m.len() == video_bytes) {
            return Ok(false);
        }
        // The times and pointers for the video's index, from the frames' index while it is there.
        let Ok(rec) = Recording::open_saved(dir) else { return Ok(false) };

        // Everything that can fail for lack of room is written before the project is touched.
        rec.write_video_index(&conv.work, video_bytes).map_err(|e| match e {
            SaveError::Io { error, .. } => reason(NOT_STORED, &error),
            _ => format!("{NOT_STORED}."),
        })?;
        file.format = state::FORMAT;
        file.store = StoreInfo { form: SavedForm::Video, video_bytes, lossless_bytes: c.frames_bytes, convert: None };
        let text = project_text(&file)?;
        // Another program (a scanner, a sync) may hold the fresh video for a moment.
        patiently(|| sync_file(&made)).map_err(|e| reason(NOT_STORED, &e))?;

        // 1. The video into the project, and on the disk there. Until the project file names
        //    it, it is a file the frames project ignores and the next start removes.
        let video = dir.join(VIDEO_FILE);
        (steps.rename)(&made, &video).map_err(|e| reason(NOT_STORED, &e))?;
        let undo = || {
            let _ = patiently(|| std::fs::remove_file(&video));
        };
        if let Err(e) = patiently(|| sync_file(&video)) {
            undo();
            return Err(reason(NOT_STORED, &e));
        }
        sync_folder(dir);

        // 2. The commit: the project file that names the video takes the old one's place. Only
        //    if the video is still the file that was verified, where the project will look for it.
        if !video_is(dir, video_bytes) {
            undo();
            eprintln!("Saved recordings: the video of a recording was changed while it was being stored, so the recording keeps its frames.");
            return Err(format!("{NOT_STORED}."));
        }
        let path = dir.join(PROJECT_FILE);
        let tmp = temp_beside(&path);
        if let Err(e) = write_whole(&tmp, text.as_bytes()).and_then(|()| (steps.rename)(&tmp, &path)) {
            let _ = std::fs::remove_file(&tmp);
            undo();
            return Err(reason(NOT_STORED, &e));
        }
        self.bump();
        // The frames go only once that rename is on the disk as far as this system can say.
        if let Err(e) = sync_file(&path) {
            eprintln!("Saved recordings: a recording was stored as video and its frames are kept until the next start ({:?}).", e.kind());
            return Ok(true);
        }
        sync_folder(dir);

        // From here on the frames and their index go. Not if the video the project now names
        // is no longer the file that was verified: then they are all there is, and they stay.
        const VIDEO_CHANGED: &str = "Saved recordings: the video of a recording was changed while it was being stored, so its frames are kept.";
        if !video_is(dir, video_bytes) {
            eprintln!("{VIDEO_CHANGED}");
            return Ok(true);
        }
        // 3. The video's index over the frames' index, which holds the same times.
        if let Err(e) = (steps.rename)(&conv.work.join(INDEX_FILE), &dir.join(INDEX_FILE)) {
            eprintln!("Saved recordings: a recording was stored as video and its frames are kept until the next start ({:?}).", e.kind());
            return Ok(true);
        }
        // 4. The frames: this project's name for them, and no other project's.
        if !video_is(dir, video_bytes) {
            eprintln!("{VIDEO_CHANGED}");
            return Ok(true);
        }
        if let Err(e) = patiently(|| std::fs::remove_file(dir.join(FRAMES_FILE))) {
            eprintln!("Saved recordings: a recording was stored as video and its frames are removed at the next start ({:?}).", e.kind());
        }
        Ok(true)
    }

    /// Note in a candidate's project file how a conversion that kept the frames ended
    /// ([`MARK_FAILED`] or [`MARK_NOT_SMALLER`]), with the codec and this version, so that it
    /// is not tried again by them. Only the project file is rewritten, in one rename, and its
    /// time of saving stays. `Ok(false)`, and nothing touched, when the project is open, being
    /// removed or no longer what the candidate describes.
    pub fn mark_conversion(&self, c: &Candidate, state: &str, codec_id: &str) -> Result<bool, String> {
        if !matches!(state, MARK_FAILED | MARK_NOT_SMALLER) {
            return Err("the end of the conversion could not be noted.".into());
        }
        let _changing = changing();
        let Some(mut file) = self.candidate_file(c) else { return Ok(false) };
        file.store.convert = Some(ConvertMark { state: state.to_string(), codec: codec_id.to_string(), app: APP_VERSION.to_string(), at: now_ms() });
        let text = project_text(&file)?;
        let path = c.dir.join(PROJECT_FILE);
        let tmp = temp_beside(&path);
        write_whole(&tmp, text.as_bytes()).and_then(|()| patient_rename(&tmp, &path)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            reason("the project file could not be written", &e)
        })?;
        self.bump();
        Ok(true)
    }

    /// Finish what a swap that was cut off left, and clear away a video nobody names: run at
    /// start-up, after [`Store::finish_removals`]. For every project folder that is not open,
    /// not being removed and not being converted:
    ///
    /// - a project kept as a video whose video is there at its size: an index of the frames
    ///   form is replaced by the video's (when the frames it describes are still there to
    ///   read it with); then, if its index reads and names this video or none (what opening
    ///   it asks), the frames file is removed, with temporary files a swap left;
    /// - a project kept as frames that passes the list's check: a video beside it is removed,
    ///   and temporary files a swap or a mark left;
    /// - a folder with no project file that holds nothing at all but a video (what a version
    ///   before video storage leaves when it deletes such a tour) is removed.
    ///
    /// Any other folder is left exactly as it is: a project this version cannot read, one from
    /// a newer version, a video project whose video is not whole, a frames project whose frames
    /// are not.
    pub fn finish_conversions(&self) {
        let _changing = changing();
        let mut changed = false;
        for (dir, site, tour, chapter) in self.chapter_folders() {
            if is_removing(&dir) || is_open(&dir) || is_converting(&dir) {
                continue;
            }
            if !is_project(&dir) {
                remove_lone_video(&dir);
                continue;
            }
            let Ok(file) = read_project(&dir) else { continue };
            let belongs = site_of(&file.server) == site && file.tour_id == tour && file.chapter_id == chapter;
            if !belongs || !payload_ok(&dir, &file) {
                continue;
            }
            changed |= match file.store.form {
                SavedForm::Video => finish_swap(&dir, file.store.video_bytes),
                SavedForm::Frames => remove_stray_video(&dir) | remove_swap_temps(&dir),
            };
        }
        if changed {
            self.bump();
        }
    }

    /// Remove what a conversion that was cut off left behind: run at start-up, with
    /// [`Store::sweep_staging`]. Real folders directly in the projects folder named
    /// `.converting-<time>-<random>` exactly, whose time (the one in the name) is at least
    /// `age` ago, and which are not the folder of the conversion that runs.
    pub fn sweep_converting(&self, age: Duration) {
        self.sweep_converting_at(age, SystemTime::now());
    }

    fn sweep_converting_at(&self, age: Duration, now: SystemTime) {
        let _changing = changing();
        let now = now.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        // By its name, which no other folder has: the same folder can be spelled in more than one way.
        let running = converting().as_ref().and_then(|slot| slot.work.file_name().map(|name| name.to_os_string()));
        for (name, dir) in folders_in(&self.root) {
            if converting_started(&name).is_some_and(|started| now >= u128::from(started) + age.as_millis()) && running.as_deref() != Some(std::ffi::OsStr::new(&name)) {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }

    /// Remove the projects of one tour on one site, and say how many. One that is open in an
    /// editor is counted, is no longer listed, and is removed when that editor closes it (or by
    /// [`Store::finish_removals`] at the next start). Only files the store wrote are removed; a
    /// folder that still holds something else is left with it.
    pub fn remove_tour(&self, server: &str, tour_id: &str) -> Result<usize, String> {
        if !is_folder_id(tour_id) {
            return Ok(0);
        }
        let site_dir = self.root.join(site_of(server));
        let tour_dir = site_dir.join(tour_id);
        // A conversion reads the frames of the project it works on: it stops first.
        if converting_now().is_some_and(|dir| dir.parent() == Some(tour_dir.as_path())) {
            for (_, dir) in folders_in(&tour_dir) {
                cancel_conversion_of(&dir, LET_GO);
            }
        }
        let _changing = changing();
        let (mut removed, mut touched, mut failed) = (0, false, None);
        for (chapter, dir) in folders_in(&tour_dir) {
            if !is_folder_id(&chapter) {
                continue;
            }
            let was_listed = is_project(&dir) && !is_removing(&dir);
            touched = true;
            // An open project is only marked: its editor still reads it.
            let done = if is_open(&dir) { mark_removing(&dir) } else { remove_files(&dir) };
            match done {
                Ok(()) => removed += usize::from(was_listed),
                Err(e) => failed = failed.or(Some(e)),
            }
        }
        // Folders the store made, when nothing is left in them.
        let _ = std::fs::remove_dir(&tour_dir);
        let _ = std::fs::remove_dir(&site_dir);
        if touched {
            self.bump();
        }
        match failed {
            Some(e) => Err(reason("a saved recording of this tour could not be removed", &e)),
            None => Ok(removed),
        }
    }

    /// Remove the project of one chapter, by its folder: a [`ProjectInfo::dir`] of this store's
    /// own list, so that a project whose file names no address (a damaged one), or one saved by
    /// a newer Mythicshot, can be removed too. As [`Store::remove_tour`] for that one folder: a
    /// project that is open in an editor is only marked, and only files the store wrote are
    /// removed.
    ///
    /// `dir` must be this store's folder `<site>/<tour>/<chapter>` with the names the store
    /// gives, spelled without a way round, and a real folder below real folders. Anything else
    /// is refused with an error and nothing is touched. A folder with such a name that is not
    /// there, or holds no project, is [`Removed::NotThere`].
    pub fn remove_project(&self, dir: &Path) -> Result<Removed, String> {
        const NOT_REMOVED: &str = "this saved recording could not be removed";
        let refused = || {
            eprintln!("Saved recordings: {NOT_REMOVED} (the folder is not a project of this store).");
            Err(format!("{NOT_REMOVED}."))
        };
        // The names, from the path as it is spelled: exactly three below the projects folder.
        let Ok(below) = dir.strip_prefix(&self.root) else { return refused() };
        let mut names = Vec::new();
        for part in below.components() {
            match part {
                std::path::Component::Normal(name) => names.push(name.to_str()),
                _ => return refused(),
            }
        }
        let [Some(site), Some(tour), Some(chapter)] = names[..] else { return refused() };
        if !(is_random_name(site) && is_folder_id(tour) && is_folder_id(chapter)) {
            return refused();
        }
        // From here on only the path built from the checked names is used.
        let site_dir = self.root.join(site);
        let tour_dir = site_dir.join(tour);
        let dir = tour_dir.join(chapter);

        // A conversion reads the frames of the project it works on: it stops first.
        cancel_conversion_of(&dir, LET_GO);
        let _changing = changing();
        // Real folders all the way down: a link or a junction could lead anywhere.
        for level in [&site_dir, &tour_dir, &dir] {
            match std::fs::symlink_metadata(level) {
                Err(_) => return Ok(Removed::NotThere),
                Ok(meta) if !meta.file_type().is_dir() => return refused(),
                Ok(_) => {}
            }
        }
        // And where the folder really is, is that place in this store's folder.
        // Failing closed is deliberate: a place that cannot be resolved, or is spelled otherwise on the disk, is refused.
        match (std::fs::canonicalize(&self.root), std::fs::canonicalize(&dir)) {
            (Ok(root), Ok(real)) if real == root.join(site).join(tour).join(chapter) => {}
            _ => return refused(),
        }
        let marked = is_removing(&dir);
        let was_listed = is_project(&dir) && !marked;
        if !was_listed && !marked {
            // No project, and no removal to finish: whatever the folder holds is not the store's to take.
            return Ok(Removed::NotThere);
        }
        // An open project is only marked: its editor still reads it.
        let open = is_open(&dir);
        let done = if open { mark_removing(&dir) } else { remove_files(&dir) };
        if !open {
            // Folders the store made, when nothing is left in them.
            prune_parents(&dir);
        }
        if !(open && marked) {
            self.bump();
        }
        match done {
            Err(e) => Err(reason(NOT_REMOVED, &e)),
            Ok(()) if open => Ok(Removed::WhenItsEditorCloses),
            Ok(()) if was_listed => Ok(Removed::Now),
            Ok(()) => Ok(Removed::NotThere),
        }
    }

    /// Remove what a save that was cut off left behind: run at start-up, last. Real folders
    /// directly in the projects folder named `.staging-<time>-<random>` exactly, whose time (the
    /// one the save wrote into the name, not the folder's own) is at least `age` ago.
    pub fn sweep_staging(&self, age: Duration) {
        self.sweep_staging_at(age, SystemTime::now());
    }

    fn sweep_staging_at(&self, age: Duration, now: SystemTime) {
        let _changing = changing();
        let now = now.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        for (name, dir) in folders_in(&self.root) {
            if staging_started(&name).is_some_and(|started| now >= u128::from(started) + age.as_millis()) {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }

    /// [`read_info`] for the codec this store reads marks for.
    fn info(&self, dir: &Path, site: &str, tour: &str, chapter: &str, server: Option<&str>) -> Option<ProjectInfo> {
        read_info(dir, site, tour, chapter, server, self.codec_id().as_deref())
    }

    /// The project's folder, or None for an id that cannot be a folder name.
    fn project_dir(&self, key: &ProjectKey) -> Option<PathBuf> {
        (is_folder_id(&key.tour_id) && is_folder_id(&key.chapter_id)).then(|| self.root.join(site_of(&key.server)).join(&key.tour_id).join(&key.chapter_id))
    }
}

impl OpenGuard {
    /// None when the project is already open.
    fn register(dir: &Path, epoch: &'static AtomicU64) -> Option<OpenGuard> {
        let name = registry_name(dir);
        registry().insert(name.clone()).then(|| OpenGuard { dir: dir.to_path_buf(), name, epoch })
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        let _changing = changing();
        registry().remove(&self.name);
        // Its tour was removed while it was open.
        if is_removing(&self.dir) {
            let _ = remove_files(&self.dir);
            prune_parents(&self.dir);
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// One save over a project, one removal, or one placing of a new project at a time, in this process.
fn changing() -> MutexGuard<'static, ()> {
    static CHANGING: Mutex<()> = Mutex::new(());
    CHANGING.lock().unwrap_or_else(|e| e.into_inner())
}

/// The projects open in an editor, by folder.
fn registry() -> MutexGuard<'static, HashSet<PathBuf>> {
    static OPEN: std::sync::LazyLock<Mutex<HashSet<PathBuf>>> = std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));
    OPEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// One name for a folder however its path is spelled.
fn registry_name(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn is_open(dir: &Path) -> bool {
    registry().contains(&registry_name(dir))
}

/// The conversion that runs.
struct Slot {
    /// The project's folder as the registry of open projects names one, and as it was handed in.
    name: PathBuf,
    dir: PathBuf,
    /// The conversion's own folder.
    work: PathBuf,
    cancel: Arc<AtomicBool>,
}

/// The one conversion of this process. Taken after [`changing`] when both are, never before.
fn converting() -> MutexGuard<'static, Option<Slot>> {
    static CONVERTING_NOW: Mutex<Option<Slot>> = Mutex::new(None);
    CONVERTING_NOW.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_converting(dir: &Path) -> bool {
    let name = registry_name(dir);
    converting().as_ref().is_some_and(|slot| slot.name == name)
}

/// The folder of the project that is being converted to a video right now, as its
/// [`ProjectInfo::dir`] names it.
pub fn converting_now() -> Option<PathBuf> {
    converting().as_ref().map(|slot| slot.dir.clone())
}

/// Tell the conversion of the project in `dir` to stop, if that is the one that runs, and wait
/// up to `wait` for it to let go of the project ([`Converting`] dropped). A conversion that was
/// told to stop is never swapped in. Does nothing for any other folder.
pub fn cancel_conversion_of(dir: &Path, wait: Duration) {
    let name = registry_name(dir);
    let until = Instant::now() + wait;
    loop {
        match converting().as_ref() {
            Some(slot) if slot.name == name => slot.cancel.store(true, Ordering::SeqCst),
            _ => return,
        }
        if Instant::now() >= until {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Converting {
    /// The conversion's own folder, in the projects folder: where the video is made and verified.
    // Only the tests ask where it is: a conversion needs no more of it than `video_path`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn work_dir(&self) -> &Path {
        &self.work
    }

    /// Where [`Store::swap_to_video`] takes the finished video from: `frames.mkv` in
    /// [`Converting::work_dir`].
    pub fn video_path(&self) -> PathBuf {
        self.work.join(VIDEO_FILE)
    }

    /// Whether the conversion was told to stop: its project was opened, saved over or
    /// removed, or [`cancel_conversion_of`] was called for it.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

impl Drop for Converting {
    fn drop(&mut self) {
        {
            let mut slot = converting();
            if slot.as_ref().is_some_and(|slot| slot.work == self.work) {
                *slot = None;
            }
        }
        // Entirely this conversion's own: a folder it made, under a name no other call gets.
        if is_real_folder(&self.work) {
            let _ = std::fs::remove_dir_all(&self.work);
        }
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

/// Why a project that keeps its recording as a video was not opened, when nothing is known to
/// be wrong with the project file.
enum NotExpanded {
    NeedsFfmpeg,
    Cancelled,
    NoRoom,
    VideoDamaged,
}

/// The one place the four outcomes become a [`LoadError`].
fn not_expanded(why: NotExpanded) -> LoadError {
    match why {
        NotExpanded::NeedsFfmpeg => LoadError::NeedsFfmpeg,
        NotExpanded::Cancelled => LoadError::Cancelled,
        NotExpanded::NoRoom => LoadError::NoRoom,
        NotExpanded::VideoDamaged => LoadError::VideoDamaged,
    }
}

/// What an expansion that did not make a recording means for the one who asked. Nothing of it
/// is ever written down: a video that could not be read now may be read the next time.
fn expand_failed(e: ExpandError) -> LoadError {
    let no_room = |e: &io::Error| matches!(e.kind(), io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded);
    match e {
        ExpandError::Cancelled | ExpandError::Codec(CodecError::Cancelled) => not_expanded(NotExpanded::Cancelled),
        ExpandError::NoRoom => not_expanded(NotExpanded::NoRoom),
        // The program could not be started, or cannot do this: as good as not there.
        ExpandError::Codec(CodecError::Failed { exit: None } | CodecError::Io(_)) => not_expanded(NotExpanded::NeedsFfmpeg),
        ExpandError::Damaged(_) | ExpandError::Codec(_) => not_expanded(NotExpanded::VideoDamaged),
        ExpandError::Io(e) if no_room(&e) => not_expanded(NotExpanded::NoRoom),
        ExpandError::Io(e) => {
            eprintln!("Saved recordings: the folder to open a recording in could not be used ({:?}).", e.kind());
            LoadError::Damaged("the folder to open the recording in could not be used".into())
        }
    }
}

/// For a video project whose video is whole: what a swap had left to do. Whether anything changed.
fn finish_swap(dir: &Path, video_bytes: u64) -> bool {
    let mut changed = false;
    // The index of the frames form holds the times; it can be read as long as its frames are there.
    if mythicshot_recording::form_of(dir) == Ok(Form::Frames)
        && let Ok(rec) = Recording::open_saved(dir)
    {
        changed |= rec.write_video_index(dir, video_bytes).is_ok();
    }
    // The frames go only from a project that opens as a video: the same look at the index as
    // when one is opened. One that would not open keeps every file it has.
    if !open_video_index(dir).is_ok_and(|index| index.video_bytes.is_none_or(|noted| noted == video_bytes)) {
        return changed;
    }
    let frames = dir.join(FRAMES_FILE);
    if std::fs::symlink_metadata(&frames).is_ok_and(|m| m.is_file()) {
        changed |= patiently(|| std::fs::remove_file(&frames)).is_ok();
    }
    changed | remove_swap_temps(dir)
}

/// For a frames project that is whole: the video a swap put beside it before it was cut off.
fn remove_stray_video(dir: &Path) -> bool {
    let video = dir.join(VIDEO_FILE);
    if !std::fs::symlink_metadata(&video).is_ok_and(|m| m.is_file()) {
        return false;
    }
    patiently(|| std::fs::remove_file(&video)).is_ok()
}

/// The temporary files of the store's own top files in a project folder.
fn remove_swap_temps(dir: &Path) -> bool {
    let mut removed = false;
    for (name, path) in files_in(dir) {
        if temp_of(&name).is_some() && is_top_file(&name) {
            removed |= std::fs::remove_file(path).is_ok();
        }
    }
    removed
}

/// A chapter folder with no project file that holds one thing only, a video: remove it, and
/// the tour's and the site's folder when they are empty. A folder with anything else in it,
/// whatever it is, is left alone.
fn remove_lone_video(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let entries: Vec<_> = entries.collect();
    let [Ok(only)] = &entries[..] else { return };
    if only.file_name() == VIDEO_FILE && only.file_type().is_ok_and(|kind| kind.is_file()) && patiently(|| std::fs::remove_file(only.path())).is_ok() {
        let _ = std::fs::remove_dir(dir);
        prune_parents(dir);
    }
}

/// Make sure what was written to the file at `path` is on the disk.
fn sync_file(path: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new().write(true).open(path)?.sync_all()
}

/// Ask the system to put a folder's entries on the disk, as far as it lets a program ask.
/// What it refuses is not an error: the file's own data was synced, and the order of the
/// steps does not depend on this.
fn sync_folder(dir: &Path) {
    #[cfg(windows)]
    let folder = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS: without it a folder cannot be opened at all.
        std::fs::OpenOptions::new().write(true).custom_flags(0x0200_0000).open(dir)
    };
    #[cfg(not(windows))]
    let folder = std::fs::File::open(dir);
    if let Ok(folder) = folder {
        let _ = folder.sync_all();
    }
}

/// A rename that is tried again while another program has the file open.
fn patient_rename(from: &Path, to: &Path) -> io::Result<()> {
    patiently(|| std::fs::rename(from, to))
}

fn is_project(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(PROJECT_FILE)).is_ok_and(|m| m.is_file())
}

/// Whether the folder carries the mark of a removal that is not finished.
fn is_removing(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(REMOVING_FILE)).is_ok()
}

/// Write the removal mark, on the disk.
fn mark_removing(dir: &Path) -> io::Result<()> {
    if is_removing(dir) { Ok(()) } else { write_whole(&dir.join(REMOVING_FILE), b"") }
}

/// The tour's and the site's folder, when nothing is left in them.
fn prune_parents(dir: &Path) {
    for parent in dir.ancestors().skip(1).take(2) {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Save over a project that an editor has open. The frames file is that editor's own and is not
/// written; the new pictures go beside the old ones, the project file is replaced in one rename,
/// and then what it no longer names is removed. A failure removes the files this call made and
/// nothing else, so the project is as it was. None when the project was closed meanwhile.
fn save_in_place(dir: &Path, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<Option<(ProjectFile, Vec<String>)>, String> {
    // Reading the index and encoding the pictures need no lock: the editor holds the project open.
    // What the project file says of the form, the sizes and a conversion is carried over. A file
    // that cannot be read says nothing, and the project is taken for frames as it always was.
    let before = match read_project(dir) {
        Ok(file) => Some(file),
        Err(Unreadable::Newer) => return Err("a newer version of Mythicshot saved this tour's recording, so it is left as it is.".into()),
        Err(_) => None,
    };
    let store = before.as_ref().map(|file| file.store.clone()).unwrap_or_default();
    let frames_bytes = match (&before, store.form) {
        (Some(file), SavedForm::Video) => {
            // The frames an editor has of a video project are its own expansion, with other
            // offsets than any file here: the recording is the project's when it was expanded
            // from the project's video and has the times the project's index names.
            let index = open_video_index(dir).map_err(|_| "the saved recording's video can no longer be read.".to_string())?;
            let from_here = snap.rec.expanded_from().is_some_and(|(video, bytes)| bytes == store.video_bytes && video.file_name().is_some_and(|name| name == VIDEO_FILE) && video.parent().is_some_and(|parent| registry_name(parent) == registry_name(dir)));
            if !(from_here && snap.rec.same_timing(&index)) {
                return Err("this tour's saved recording is open in an editor and belongs to another recording.".into());
            }
            if !video_is(dir, store.video_bytes) {
                return Err("the saved recording's video can no longer be read.".into());
            }
            file.frames_bytes
        }
        _ => {
            let kept = Recording::open_saved(dir).map_err(|_| "the saved recording's frames can no longer be read.".to_string())?;
            if !kept.same_frames(snap.rec) {
                return Err("this tour's saved recording is open in an editor and belongs to another recording.".into());
            }
            std::fs::metadata(dir.join(FRAMES_FILE)).map_err(|e| reason("the saved recording's frames can no longer be read", &e))?.len()
        }
    };
    let (file, blobs, left_out) = state::to_file(key, snap, now_ms(), frames_bytes, store.clone());
    let text = project_text(&file)?;

    let _changing = changing();
    if !(is_project(dir) && is_open(dir)) {
        return Ok(None);
    }
    // Closed, replaced or converted, and opened again meanwhile: this is no longer what was checked.
    let said = |file: &Option<ProjectFile>| file.as_ref().map(|file| (file.saved_at, file.store.clone()));
    let kept_whole = match store.form {
        SavedForm::Frames => frames_are(dir, frames_bytes),
        SavedForm::Video => video_is(dir, store.video_bytes),
    };
    if !kept_whole || said(&read_project(dir).ok()) != said(&before) {
        return Err("the saved recording's frames changed while it was being saved.".into());
    }
    let had_pictures = std::fs::symlink_metadata(dir.join(PICTURES)).is_ok();
    let mut made = Vec::new();
    let mut written = Ok(());
    for blob in &blobs {
        match write_blob(dir, blob) {
            Ok(new) => made.extend(new),
            Err(e) => {
                written = Err(e);
                break;
            }
        }
    }
    let written = written.and_then(|()| {
        let path = dir.join(PROJECT_FILE);
        let tmp = temp_beside(&path);
        write_whole(&tmp, text.as_bytes()).and_then(|()| (steps.rename)(&tmp, &path)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            reason("the project file could not be written", &e)
        })
    });
    match written {
        Ok(()) => {
            tidy(dir, &blobs.iter().map(|b| b.name.clone()).collect());
            // Saved again after it was removed while open: the save is the later word, so it is kept.
            let _ = std::fs::remove_file(dir.join(REMOVING_FILE));
            Ok(Some((file, left_out)))
        }
        Err(e) => {
            for path in made {
                let _ = std::fs::remove_file(path);
            }
            if !had_pictures {
                let _ = std::fs::remove_dir(dir.join(PICTURES));
            }
            Err(e)
        }
    }
}

/// Put everything a project holds into the new folder `staging`, the project file last.
fn fill(staging: &Path, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<(ProjectFile, Vec<String>), String> {
    let kept = (steps.keep_frames)(snap.rec, staging).map_err(frames_reason)?;
    let (frames_bytes, store) = match kept.form {
        Form::Frames => (kept.bytes, StoreInfo::default()),
        // A recording that was expanded from a video and is kept as that video: the project is
        // born in the video form. What its frames took is what the project the video comes
        // from notes for them, when that can still be read; else it is not known, and is 0.
        Form::Video => {
            let source = snap.rec.expanded_from().and_then(|(video, _)| video.parent()).and_then(|from| read_project(from).ok());
            let lossless = source.filter(|file| file.store.form == SavedForm::Video && file.store.video_bytes == kept.bytes).map_or(0, |file| file.store.lossless_bytes);
            (lossless, StoreInfo { form: SavedForm::Video, video_bytes: kept.bytes, lossless_bytes: lossless, convert: None })
        }
    };
    let (file, blobs, left_out) = state::to_file(key, snap, now_ms(), frames_bytes, store);
    let text = project_text(&file)?;
    for blob in &blobs {
        write_blob(staging, blob)?;
    }
    write_whole(&staging.join(PROJECT_FILE), text.as_bytes()).map_err(|e| reason("the project file could not be written", &e))?;
    Ok((file, left_out))
}

/// Take a replaced project's folder away once the new project is in `dir`: the files the store
/// wrote go, a file or folder somebody else had put beside them moves along when its name is
/// free, and the folder goes only when it is empty. A link is never moved into the new project.
fn retire(aside: &Path, dir: &Path) {
    let _ = remove_files(aside);
    for entry in std::fs::read_dir(aside).into_iter().flatten().flatten() {
        let to = dir.join(entry.file_name());
        let real = entry.file_type().is_ok_and(|kind| kind.is_file() || kind.is_dir());
        if real && std::fs::symlink_metadata(&to).is_err() {
            let _ = std::fs::rename(entry.path(), to);
        }
    }
    let _ = std::fs::remove_dir(aside);
}

/// The address, tour and chapter a folder's project file names, and when it says it was saved
/// (0 when it does not say); also for a file this version cannot open.
fn named_key(dir: &Path) -> Option<(ProjectKey, i64)> {
    let text = project_file_text(dir).ok()?;
    let value: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let word = |name: &str| value.get(name).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    let saved_at = value.get("saved_at").and_then(|v| v.as_i64()).unwrap_or(0);
    Some((ProjectKey { server: word("server")?, tour_id: word("tour_id")?, chapter_id: word("chapter_id")? }, saved_at))
}

fn project_text(file: &ProjectFile) -> Result<String, String> {
    let text = state::to_text(file).map_err(|_| "the project could not be written down.".to_string())?;
    if text.len() > MAX_PROJECT_JSON_BYTES {
        return Err("the project is too large to be saved.".into());
    }
    Ok(text)
}

/// Write one picture or logo: under a temporary name, on the disk, then renamed. A file of that
/// name is trusted only when it has the right size; one cut short by an earlier save is replaced.
/// Returns the file's path when there was no file of that name before.
fn write_blob(dir: &Path, blob: &Blob) -> Result<Option<PathBuf>, String> {
    let failed = |e: io::Error| reason("a picture of the project could not be written", &e);
    let path = match blob.name.strip_prefix("pictures/") {
        Some(file) if state::is_picture_name(&blob.name) => {
            let pictures = dir.join(PICTURES);
            if std::fs::symlink_metadata(&pictures).is_ok() && !is_real_folder(&pictures) {
                return Err("the project's pictures folder is not a folder.".into());
            }
            std::fs::create_dir_all(&pictures).map_err(failed)?;
            pictures.join(file)
        }
        None if state::is_logo_name(&blob.name) => dir.join(&blob.name),
        _ => return Err("a picture of the project has a name that cannot be used.".into()),
    };
    let before = std::fs::symlink_metadata(&path).ok();
    if before.as_ref().is_some_and(|m| m.is_file() && m.len() == blob.bytes.len() as u64) {
        return Ok(None);
    }
    let tmp = temp_beside(&path);
    write_whole(&tmp, &blob.bytes).and_then(|()| patiently(|| std::fs::rename(&tmp, &path))).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        failed(e)
    })?;
    Ok(before.is_none().then_some(path))
}

/// Write the new file `path`, whole and on the disk.
fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// `<name>.<random>.tmp` beside `path`.
fn temp_beside(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", random_name()));
    path.with_file_name(name)
}

/// What a temporary file is to become: `frames.zst` for `frames.zst.<letters, digits, dashes>.tmp`.
/// The store's own temporary files and the recording's are both named so.
fn temp_of(name: &str) -> Option<&str> {
    let (base, mark) = name.strip_suffix(".tmp")?.rsplit_once('.')?;
    (!mark.is_empty() && mark.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')).then_some(base)
}

/// A file directly in a project folder that only the store or the recording puts there.
fn is_top_file(name: &str) -> bool {
    let known = |n: &str| matches!(n, PROJECT_FILE | INDEX_FILE | FRAMES_FILE | VIDEO_FILE) || state::is_logo_name(n);
    name == REMOVING_FILE || known(name) || temp_of(name).is_some_and(known)
}

/// A file in a project's `pictures` folder that only the store puts there.
fn is_picture_file(name: &str) -> bool {
    let known = |n: &str| state::is_picture_name(&format!("pictures/{n}"));
    known(name) || temp_of(name).is_some_and(known)
}

/// The real files in a project's `pictures` folder; none when that is a link and not a folder.
fn pictures_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let pictures = dir.join(PICTURES);
    if is_real_folder(&pictures) { files_in(&pictures) } else { Vec::new() }
}

/// Remove the `pictures` folder if it is a real folder and empty.
fn remove_empty_pictures(dir: &Path) {
    let pictures = dir.join(PICTURES);
    if is_real_folder(&pictures) {
        let _ = std::fs::remove_dir(pictures);
    }
}

/// A folder, and not a link or a junction to one.
fn is_real_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

/// After a save over a project: remove the pictures and logos that are not in `keep`, and
/// temporary files a cut-off save left. Nothing else is touched, and a file that cannot be
/// removed stays.
fn tidy(dir: &Path, keep: &BTreeSet<String>) {
    for (name, path) in files_in(dir) {
        let stale = temp_of(&name).is_some() && is_top_file(&name);
        if stale || (state::is_logo_name(&name) && !keep.contains(&name)) {
            let _ = std::fs::remove_file(path);
        }
    }
    for (name, path) in pictures_in(dir) {
        if is_picture_file(&name) && !keep.contains(&format!("pictures/{name}")) {
            let _ = std::fs::remove_file(path);
        }
    }
    remove_empty_pictures(dir);
}

/// Remove a project: the removal mark is written, then its project file goes, so that what is
/// left is no project, then every other file the store wrote, the mark last and only when all of
/// them went, then the folders if they are empty. A file somebody else put there stays, with its
/// folder, and is not an error.
fn remove_files(dir: &Path) -> io::Result<()> {
    // Without the mark the removal still happens; it only could not be finished after a crash.
    let _ = mark_removing(dir);
    match patiently(|| std::fs::remove_file(dir.join(PROJECT_FILE))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let ours = files_in(dir).into_iter().filter(|(name, _)| is_top_file(name) && name != REMOVING_FILE).chain(pictures_in(dir).into_iter().filter(|(name, _)| is_picture_file(name)));
    let mut failed = None;
    for (_, path) in ours {
        match patiently(|| std::fs::remove_file(&path)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => failed = failed.or(Some(e)),
            _ => {}
        }
    }
    if let Some(e) = failed {
        return Err(e);
    }
    remove_empty_pictures(dir);
    let _ = std::fs::remove_file(dir.join(REMOVING_FILE));
    let _ = std::fs::remove_dir(dir);
    Ok(())
}

/// The real files directly in `dir`, by name. A link is not followed.
fn files_in(dir: &Path) -> Vec<(String, PathBuf)> {
    entries_in(dir, |kind| kind.is_file())
}

/// The real folders directly in `dir`, by name. A link is not followed.
fn folders_in(dir: &Path) -> Vec<(String, PathBuf)> {
    entries_in(dir, |kind| kind.is_dir())
}

fn entries_in(dir: &Path, wanted: impl Fn(&std::fs::FileType) -> bool) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().filter(|e| e.file_type().is_ok_and(|kind| wanted(&kind))).filter_map(|e| Some((e.file_name().into_string().ok()?, e.path()))).collect()
}

/// The chapter folders of the site folder `site_dir`, added to `out` with their names.
fn chapters_of_site(site_dir: &Path, site: &str, out: &mut Vec<(PathBuf, String, String, String)>) {
    for (tour, tour_dir) in folders_in(site_dir).into_iter().filter(|(tour, _)| is_folder_id(tour)) {
        for (chapter, dir) in folders_in(&tour_dir).into_iter().filter(|(chapter, _)| is_folder_id(chapter)) {
            out.push((dir, site.to_string(), tour.clone(), chapter));
        }
    }
}

fn sorted(mut list: Vec<ProjectInfo>) -> Vec<ProjectInfo> {
    list.sort_by(|a, b| a.dir.cmp(&b.dir));
    list
}

/// The size of a project's files together.
fn bytes_in(dir: &Path) -> u64 {
    files_in(dir).into_iter().chain(pictures_in(dir)).filter_map(|(_, path)| std::fs::symlink_metadata(path).ok()).fold(0, |sum, m| sum.saturating_add(m.len()))
}

/// Whether the frames file is a file of this many bytes.
fn frames_are(dir: &Path, bytes: u64) -> bool {
    std::fs::symlink_metadata(dir.join(FRAMES_FILE)).is_ok_and(|m| m.is_file() && m.len() == bytes)
}

/// Whether the video is a file of this many bytes, and of any at all: a project file that
/// names no size for its video names no video.
fn video_is(dir: &Path, bytes: u64) -> bool {
    bytes > 0 && std::fs::symlink_metadata(dir.join(VIDEO_FILE)).is_ok_and(|m| m.is_file() && m.len() == bytes)
}

fn has_index(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(INDEX_FILE)).is_ok_and(|m| m.is_file())
}

/// The cheap check of what a project file names: the frames file, or the video, is there at
/// the size the file says, and so is an index. Neither is read.
fn payload_ok(dir: &Path, file: &ProjectFile) -> bool {
    let kept = match file.store.form {
        SavedForm::Frames => frames_are(dir, file.frames_bytes),
        SavedForm::Video => video_is(dir, file.store.video_bytes),
    };
    kept && has_index(dir)
}

fn form_of_store(store: &StoreInfo) -> Form {
    match store.form {
        SavedForm::Frames => Form::Frames,
        SavedForm::Video => Form::Video,
    }
}

/// How the try the mark notes ended, when it was made with this codec by this version.
fn mark_holds(store: &StoreInfo, codec: &str) -> Option<ConvertState> {
    let mark = store.convert.as_ref().filter(|mark| mark.codec == codec && mark.app == APP_VERSION)?;
    match mark.state.as_str() {
        MARK_FAILED => Some(ConvertState::Failed),
        MARK_NOT_SMALLER => Some(ConvertState::NotSmaller),
        _ => None,
    }
}

/// What a project file says about storing its recording as video, for the codec in use.
fn convert_state(store: &StoreInfo, codec: Option<&str>) -> ConvertState {
    match store.form {
        SavedForm::Video => ConvertState::Done,
        SavedForm::Frames => codec.and_then(|codec| mark_holds(store, codec)).unwrap_or(ConvertState::NotTried),
    }
}

/// Why a project file could not be read.
enum Unreadable {
    NotThere,
    Newer,
    Damaged(String),
}

/// The text of a folder's project file; its size is looked at before it is read.
fn project_file_text(dir: &Path) -> Result<String, Unreadable> {
    let path = dir.join(PROJECT_FILE);
    let meta = std::fs::symlink_metadata(&path).map_err(|_| Unreadable::NotThere)?;
    if !meta.is_file() {
        return Err(Unreadable::NotThere);
    }
    if meta.len() > MAX_PROJECT_JSON_BYTES as u64 {
        return Err(Unreadable::Damaged("the project file is too large".into()));
    }
    std::fs::read_to_string(&path).map_err(|e| Unreadable::Damaged(e.to_string()))
}

fn read_project(dir: &Path) -> Result<ProjectFile, Unreadable> {
    parse_project(&project_file_text(dir)?).map_err(|e| match e {
        ParseError::Newer(_) => Unreadable::Newer,
        ParseError::Damaged(why) => Unreadable::Damaged(why),
    })
}

/// [`state::parse`], and a conversion mark that does not have a mark's shape costs only
/// itself: the file is read as if it carried none.
fn parse_project(text: &str) -> Result<ProjectFile, ParseError> {
    match state::parse(text) {
        Err(ParseError::Damaged(why)) => without_mark(text).and_then(|text| state::parse(&text).ok()).ok_or(ParseError::Damaged(why)),
        other => other,
    }
}

/// The project file's text without its conversion mark; None when it carries none.
fn without_mark(text: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    value.get_mut("store")?.as_object_mut()?.remove("convert")?;
    serde_json::to_string(&value).ok()
}

/// What the list shows for the project in `dir`, the folder `<site>/<tour>/<chapter>`; None when
/// the folder holds no project file, or is being removed. The cheap check: the project file
/// parses, its address, tour and chapter are the folder's, and the frames file, or the video of
/// a project kept as one, is there at the size the file names. `codec` is the codec a
/// conversion mark is read for.
fn read_info(dir: &Path, site: &str, tour: &str, chapter: &str, server: Option<&str>, codec: Option<&str>) -> Option<ProjectInfo> {
    if is_removing(dir) {
        return None;
    }
    let text = match project_file_text(dir) {
        Err(Unreadable::NotThere) => return None,
        other => other.ok(),
    };
    let parsed = text.as_deref().map(parse_project);
    // What a row shows is read even from a file that cannot be opened, when it is there to read.
    let loose: Option<serde_json::Value> = match &parsed {
        Some(Err(_)) => text.as_deref().and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok()),
        _ => None,
    };
    let word = |name: &str| loose.as_ref().and_then(|v| v.get(name)).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let loose_saved_at = loose.as_ref().and_then(|v| v.get("saved_at")).and_then(|v| v.as_i64()).unwrap_or(0);
    // The form a file that cannot be opened seems to name; it decides nothing.
    let loose_form = match loose.as_ref().and_then(|v| v.get("store")).and_then(|v| v.get("form")).and_then(|v| v.as_str()) {
        Some("video") => Form::Video,
        _ => Form::Frames,
    };
    let frames_bytes = match &parsed {
        Some(Ok(file)) => file.frames_bytes,
        _ => 0,
    };
    let (named_server, title, tour_title, saved_at, problem, form, convert) = match &parsed {
        Some(Ok(file)) => {
            let belongs = site_of(&file.server) == site && file.tour_id == tour && file.chapter_id == chapter;
            let problem = if !belongs {
                Some(Problem::Damaged)
            } else if !payload_ok(dir, file) {
                Some(Problem::FramesMissing)
            } else {
                None
            };
            (file.server.clone(), file.title.clone(), file.tour_title.clone(), file.saved_at, problem, form_of_store(&file.store), convert_state(&file.store, codec))
        }
        Some(Err(ParseError::Newer(_))) => (word("server"), word("title"), word("tour_title"), loose_saved_at, Some(Problem::Newer), loose_form, ConvertState::NotTried),
        _ => (word("server"), word("title"), word("tour_title"), loose_saved_at, Some(Problem::Damaged), loose_form, ConvertState::NotTried),
    };
    let server = if site_of(&named_server) == site { named_server } else { server.unwrap_or("").to_string() };
    Some(ProjectInfo { key: ProjectKey { server, tour_id: tour.to_string(), chapter_id: chapter.to_string() }, title, tour_title, saved_at, bytes: bytes_in(dir), frames_bytes, dir: dir.to_path_buf(), problem, form, convert })
}

/// The folder name of a site: the first 16 hex characters of the SHA-256 of its address.
fn site_of(server: &str) -> String {
    hex16(server.as_bytes())
}

fn hex16(bytes: &[u8]) -> String {
    let hex: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    hex[..RANDOM_CHARS].to_string()
}

/// Sixteen hex characters no other call gets: part of the name of a staging folder, a replaced
/// project's folder or a temporary file.
fn random_name() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    hex16(format!("{}-{nanos}-{}", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed)).as_bytes())
}

fn is_random_name(s: &str) -> bool {
    s.len() == RANDOM_CHARS && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// When the save that made the staging folder `name` started, in milliseconds since 1970; None
/// for any name but `.staging-<1 to 15 digits>-<16 hex>`.
fn staging_started(name: &str) -> Option<u64> {
    started_in(name, STAGING)
}

/// The same for a conversion's folder, `.converting-<1 to 15 digits>-<16 hex>`.
fn converting_started(name: &str) -> Option<u64> {
    started_in(name, CONVERTING)
}

fn started_in(name: &str, prefix: &str) -> Option<u64> {
    let (started, random) = name.strip_prefix(prefix)?.split_once('-')?;
    ((1..=15).contains(&started.len()) && started.bytes().all(|b| b.is_ascii_digit()) && is_random_name(random)).then(|| started.parse().ok()).flatten()
}

/// Whether a tour or chapter id can be a folder's name as it is: lower-case letters, digits, `_`
/// and `-` only (so no path, no drive and no two ids that differ only in case), and not a name
/// Windows keeps for a device.
fn is_folder_id(id: &str) -> bool {
    const DEVICES: [&str; 22] = ["con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9"];
    (1..=ID_MAX_CHARS).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') && !DEVICES.contains(&id)
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)).unwrap_or(0)
}

/// "what: why." as one sentence, whatever `why` ends with.
/// The reason for the user's sentence when the system refused a step: `what` could not be
/// done, or, when the disk had no room, that. The system's own text never goes into it, and
/// only the kind of error is logged: its text can hold a folder's name.
fn reason(what: &str, e: &io::Error) -> String {
    eprintln!("Saved recordings: {what} ({:?}).", e.kind());
    if matches!(e.kind(), io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded) { DISK_FULL.to_string() } else { format!("{what}.") }
}

/// The reason when the recording's frames and index could not be put into a project.
fn frames_reason(e: SaveError) -> String {
    match e {
        SaveError::Cancelled => SAVING_CANCELLED.to_string(),
        SaveError::Unusable(why) => one_sentence(why),
        SaveError::Io { step: SaveStep::Frames, error, .. } => reason("the recording's files could not be copied", &error),
        SaveError::Io { step: SaveStep::Index, error, .. } => reason("the recording's files could not be written", &error),
    }
}

/// The text with exactly one full stop at its end.
fn one_sentence(text: &str) -> String {
    format!("{}.", text.trim_end().trim_end_matches('.'))
}

/// Try a rename or a removal again while another program has the file open.
fn patiently(mut step: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut tried = 1;
    loop {
        match step() {
            Err(e) if tried < BUSY_TRIES && (e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32))) => {
                tried += 1;
                std::thread::sleep(BUSY_WAIT);
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::state::tests::{assert_same_edit, assert_same_items, edit, events, image_item, items, logo_name, logo_source, mouse_only, plain};
    use crate::video::Edit;
    use mythicshot_editor::Item;
    use mythicshot_editor::overlays::InputEvent;

    const SITE_A: &str = "https://tours.example.com";
    const SITE_B: &str = "https://other.example.com";
    const TOUR_1: &str = "tour_0123456789abcdef01234567";
    const TOUR_2: &str = "tour_89abcdef0123456701234567";
    const CH_1: &str = "ch_0123456789abcdef01234567";
    const CH_2: &str = "ch_89abcdef0123456701234567";
    const FRAMES: usize = 10;

    fn key(server: &str, tour: &str, chapter: &str) -> ProjectKey {
        ProjectKey { server: server.into(), tour_id: tour.into(), chapter_id: chapter.into() }
    }

    fn key_1() -> ProjectKey {
        key(SITE_A, TOUR_1, CH_1)
    }

    /// The bytes of a frames file of `n` frames, and the index that describes it. The store never
    /// decodes a frame, so the frames are not pictures.
    fn frames_and_index(n: usize) -> (Vec<u8>, String) {
        frames_and_index_of(n, 0)
    }

    /// The same times and no pointer, and every frame `longer` bytes longer.
    fn frames_and_index_of(n: usize, longer: usize) -> (Vec<u8>, String) {
        let (mut bytes, mut rows) = (Vec::new(), Vec::new());
        for i in 0..n {
            let frame = vec![i as u8 + 1; 20 + i + longer];
            rows.push(serde_json::json!([bytes.len(), frame.len(), i == 0, i * 100, null, null]));
            bytes.extend(frame);
        }
        (bytes, serde_json::json!({"format": 1, "width": 100, "height": 80, "end_ms": n * 100, "frames": rows}).to_string())
    }

    /// A recording of `n` frames of 100 x 80 in the folder `<root>/<name>`, as an editor has one.
    fn recorded(root: &Path, name: &str, n: usize) -> Recording {
        recorded_of(root, name, n, 0)
    }

    fn recorded_of(root: &Path, name: &str, n: usize, longer: usize) -> Recording {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let (bytes, index) = frames_and_index_of(n, longer);
        std::fs::write(dir.join(FRAMES_FILE), bytes).unwrap();
        std::fs::write(dir.join(INDEX_FILE), index).unwrap();
        Recording::open_saved(&dir).unwrap()
    }

    /// A store in a temporary folder and an editor's state to save: a recording, an edit with a
    /// cover logo, mouse and key events, and one of every kind of item (one a pasted picture).
    struct World {
        tmp: tempfile::TempDir,
        store: Store,
        rec: Recording,
        _logo_dir: tempfile::TempDir,
        edit: Edit,
        events: Vec<InputEvent>,
        items: Vec<Item>,
    }

    impl World {
        fn new() -> World {
            let tmp = tempfile::tempdir().unwrap();
            let store = own_store(tmp.path().join("projects"));
            let rec = recorded(tmp.path(), "rec", FRAMES);
            let (logo_dir, logo) = logo_source();
            let edit = edit(&rec, Some(&logo));
            World { tmp, store, rec, _logo_dir: logo_dir, edit, events: events(), items: items() }
        }

        fn snap(&self) -> Snapshot<'_> {
            Snapshot { rec: &self.rec, title: "Chapter one", tour_title: "Customer onboarding", steps: 3, edit: &self.edit, events: &self.events, items: &self.items }
        }

        fn save(&self, key: &ProjectKey) -> Saved {
            self.store.save(key, &self.snap()).unwrap()
        }

        fn dir(&self, key: &ProjectKey) -> PathBuf {
            self.store.root().join(site_of(&key.server)).join(&key.tour_id).join(&key.chapter_id)
        }
    }

    /// The error the system gives when the disk has no room.
    fn no_room() -> io::Error {
        io::Error::from_raw_os_error(if cfg!(windows) { 112 } else { 28 })
    }

    /// Whether a reason fits after "…: " in the user's sentence: it starts lower-case, is one
    /// sentence with one full stop, has no second colon, and holds no system text or folder.
    fn reads_as_a_reason(reason: &str, root: &Path) -> bool {
        reason.starts_with(|c: char| c.is_lowercase()) && reason.ends_with('.') && reason.matches('.').count() == 1 && !reason.contains(':') && !reason.contains("os error") && !reason.contains(&*root.to_string_lossy()) && !reason.contains('\\')
    }

    #[test]
    fn what_the_system_refuses_becomes_a_reason_without_the_systems_own_words() {
        let w = World::new();
        // The system's own error for a full disk, wherever in a save it comes up.
        assert_eq!(no_room().kind(), io::ErrorKind::StorageFull);
        assert_eq!(reason("the project file could not be written", &no_room()), "the disk is full.");
        assert_eq!(frames_reason(SaveError::Io { step: SaveStep::Index, context: "could not write the recording's index", error: no_room() }), "the disk is full.");
        let full = |_: &Path, _: &Path| -> io::Result<()> { Err(no_room()) };
        assert_eq!(w.store.save_with(&key_1(), &w.snap(), &Steps { keep_frames: &real_frames, rename: &full }).unwrap_err(), "the disk is full.");
        assert_eq!(tree(w.store.root()), Vec::<String>::new());

        // The recording's own files are gone: neither a link nor a copy can be made.
        let gone = recorded(w.tmp.path(), "gone", 4);
        std::fs::remove_file(w.tmp.path().join("gone").join(FRAMES_FILE)).unwrap();
        let edit = Edit::new(&gone, Default::default());
        let snap = Snapshot { rec: &gone, title: "Gone", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] };
        let err = w.store.save(&key_1(), &snap).unwrap_err();
        assert_eq!(err, "the recording's files could not be copied.");
        assert_eq!(tree(w.store.root()), Vec::<String>::new());

        // Where the folder for saved recordings should be, there is a file.
        let blocked = own_store(w.tmp.path().join("in-the-way"));
        std::fs::write(blocked.root(), b"mine").unwrap();
        let err = blocked.save(&key_1(), &w.snap()).unwrap_err();
        assert_eq!(err, "the folder for saved recordings could not be made.");
        assert_eq!(std::fs::read(blocked.root()).unwrap(), b"mine");

        // A recording that cannot be kept at all says so in its own words.
        let empty = Recording::timing_only(100, 80, &[], 0);
        let edit = Edit::new(&empty, Default::default());
        let snap = Snapshot { rec: &empty, title: "Empty", tour_title: "Customer onboarding", steps: 0, edit: &edit, events: &[], items: &[] };
        assert_eq!(w.store.save(&key_1(), &snap).unwrap_err(), "the recording has no frames to keep.");
        // Every one of them fits after the colon of the user's sentence.
        for said in ["the disk is full.", "the recording's files could not be copied.", "the folder for saved recordings could not be made.", "the recording has no frames to keep.", SAVING_CANCELLED] {
            assert!(reads_as_a_reason(said, w.tmp.path()), "{said}");
        }
    }

    #[test]
    fn a_save_stopped_while_the_frames_are_copied_is_cancelled_and_leaves_nothing() {
        let w = World::new();
        // The copy was told to stop after it had written part of the frames file.
        let stopped = |_: &Recording, dir: &Path| -> Result<SavedAs, SaveError> {
            assert!(dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with(STAGING)));
            Err(SaveError::Cancelled)
        };
        let err = w.store.save_with(&key_1(), &w.snap(), &Steps { keep_frames: &stopped, rename: &real_rename }).unwrap_err();
        assert_eq!(err, "saving was cancelled.");
        assert_eq!(tree(w.store.root()), Vec::<String>::new(), "no staging folder is left");
        assert_eq!((w.store.list(), changes(&w.store)), (vec![], 0));
        // Told to stop when the frames can be linked: nothing is copied, so the save is whole.
        let saved = w.store.save_while(&key_1(), &w.snap(), &|| false).unwrap();
        assert_eq!(w.store.list(), vec![saved.info]);
    }

    /// A store with a change counter of its own, so that what another test saves is not counted.
    fn own_store(root: PathBuf) -> Store {
        Store { root, epoch: Box::leak(Box::new(AtomicU64::new(0))), codec: Some(Some(TEST_CODEC.to_string())) }
    }

    /// The codec every store of these tests reads conversion marks for.
    const TEST_CODEC: &str = "a-codec-of-the-tests";

    fn changes(store: &Store) -> u64 {
        store.epoch.load(Ordering::SeqCst)
    }

    /// Whether a path's last part starts with `start`.
    fn named(path: &Path, start: &str) -> bool {
        path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(start))
    }

    /// The folders directly in the projects folder whose names start with `start`.
    fn in_root(store: &Store, start: &str) -> Vec<PathBuf> {
        folders_in(store.root()).into_iter().filter(|(name, _)| name.starts_with(start)).map(|(_, path)| path).collect()
    }

    /// As after a crash and a new start: the project is not open any more, and its guard never ran.
    fn forget_open(loaded: Loaded) {
        registry().remove(&loaded.open.name);
        std::mem::forget(loaded.open);
    }

    fn real_rename(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn real_frames(rec: &Recording, dir: &Path) -> Result<SavedAs, SaveError> {
        rec.save_into_with(dir, &|| true)
    }

    /// Every file and folder below `dir`, with `/`, sorted; folders end with `/`.
    fn tree(dir: &Path) -> Vec<String> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                if path.is_dir() {
                    out.push(format!("{name}/"));
                    walk(base, &path, out);
                } else {
                    out.push(name);
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }

    /// Every file below `dir` with its bytes.
    fn contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
        tree(dir).into_iter().filter(|n| !n.ends_with('/')).map(|n| (n.clone(), std::fs::read(dir.join(&n)).unwrap())).collect()
    }

    fn picture_names(dir: &Path) -> Vec<String> {
        tree(&dir.join(PICTURES))
    }

    fn load_err(store: &Store, key: &ProjectKey) -> LoadError {
        match store.load(key) {
            Ok(_) => panic!("the project opened"),
            Err(e) => e,
        }
    }

    fn loaded(store: &Store, key: &ProjectKey) -> Loaded {
        match store.load(key) {
            Ok(l) => l,
            Err(e) => panic!("the project did not open: {e:?}"),
        }
    }

    fn keys(list: &[ProjectInfo]) -> Vec<ProjectKey> {
        list.iter().map(|p| p.key.clone()).collect()
    }

    #[test]
    fn the_default_folder_is_projects_in_the_users_folder_or_nothing() {
        let local = std::env::temp_dir().join("Mythicshot").join("data");
        assert!(local.is_absolute());
        assert_eq!(Store::root_in(Some(local.clone())), Some(local.join("projects")));
        // No folder of the user's, or one without a full path: no store, never the folder the app runs in.
        assert_eq!(Store::root_in(None), None);
        assert_eq!(Store::root_in(Some(PathBuf::from("."))), None);
        assert_eq!(Store::root_in(Some(PathBuf::from("Mythicshot/data"))), None);
        if let Some(root) = Store::default_root() {
            assert!(root.is_absolute() && root.ends_with("projects"), "{root:?}");
        }
    }

    #[test]
    fn a_saved_project_loads_back_exactly_as_it_was_saved() {
        let w = World::new();
        assert_eq!(changes(&w.store), 0);
        let saved = w.save(&key_1());
        assert_eq!(changes(&w.store), 1, "a save tells the pages to read the list again");
        let dir = w.dir(&key_1());
        assert_eq!(saved.left_out, Vec::<String>::new());
        assert_eq!((saved.info.key.clone(), saved.info.title.as_str(), saved.info.tour_title.as_str(), saved.info.problem), (key_1(), "Chapter one", "Customer onboarding", None));
        assert_eq!(saved.info.dir, dir);
        assert!(saved.info.saved_at > 1_700_000_000_000);
        // The layout: the project's own folder, and nothing else in the projects folder.
        let picture = picture_names(&dir);
        assert_eq!(picture.len(), 1);
        let mut want = vec![FRAMES_FILE.to_string(), logo_name(), "pictures/".to_string(), format!("pictures/{}", picture[0]), PROJECT_FILE.to_string(), INDEX_FILE.to_string()];
        want.sort();
        assert_eq!(tree(&dir), want);
        assert_eq!(std::fs::read_dir(w.store.root()).unwrap().count(), 1, "no staging folder is left");
        assert_eq!(saved.info.bytes, contents(&dir).iter().map(|(_, b)| b.len() as u64).sum::<u64>());
        assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), frames_and_index(FRAMES).0);
        assert_eq!(w.store.list(), vec![saved.info.clone()]);

        let l = loaded(&w.store, &key_1());
        assert_eq!(l.info, saved.info);
        assert_eq!((l.rec.width, l.rec.height, l.rec.end_ms, l.rec.frames.len()), (100, 80, 1000, FRAMES));
        assert_eq!(l.rec.frames.iter().map(|m| m.t_ms).collect::<Vec<_>>(), w.rec.frames.iter().map(|m| m.t_ms).collect::<Vec<_>>());
        // The logo is now the copy inside the project.
        let mut want = w.edit.clone();
        want.tour_doc.logo_path = Some(dir.join(logo_name()));
        assert_same_edit(&l.restored.edit, &want);
        assert_same_items(&l.restored.items, &w.items);
        assert_eq!(l.restored.events, mouse_only(&w.events));
        assert_eq!((l.restored.title.as_str(), l.restored.dropped.len()), ("Chapter one", 0));
    }

    #[test]
    fn the_original_recording_can_be_deleted_and_the_project_still_loads() {
        let w = World::new();
        w.save(&key_1());
        std::fs::remove_dir_all(w.tmp.path().join("rec")).unwrap();
        let l = loaded(&w.store, &key_1());
        assert_eq!(std::fs::read(l.info.dir.join(FRAMES_FILE)).unwrap(), frames_and_index(FRAMES).0);
    }

    #[test]
    fn a_save_that_fails_while_the_frames_are_copied_leaves_no_project_and_no_staging_folder() {
        let w = World::new();
        // The disk fills: half a frames file is written, then the copy stops.
        let full_disk = |_: &Recording, dir: &Path| -> Result<SavedAs, SaveError> {
            std::fs::write(dir.join("frames.zst.1-2.tmp"), [1u8; 40]).unwrap();
            Err(SaveError::Io { step: SaveStep::Frames, context: "could not keep the frames", error: no_room() })
        };
        let err = w.store.save_with(&key_1(), &w.snap(), &Steps { keep_frames: &full_disk, rename: &real_rename }).unwrap_err();
        assert_eq!(err, "the disk is full.");
        assert_eq!(changes(&w.store), 0, "a save that failed is no change");
        assert!(err.ends_with('.') && !err.contains(&*w.tmp.path().to_string_lossy()) && !err.contains(STAGING), "{err}");
        assert_eq!(tree(w.store.root()), Vec::<String>::new());
        assert_eq!(w.store.list(), vec![]);
        assert_eq!(load_err(&w.store, &key_1()), LoadError::NotSaved);
        // The next save, with room on the disk, goes through.
        w.save(&key_1());
        assert_eq!(w.store.list().len(), 1);
    }

    #[test]
    fn a_first_save_that_cannot_be_put_in_place_leaves_no_project_and_no_staging_folder() {
        let w = World::new();
        let refuse = |_: &Path, _: &Path| -> io::Result<()> { Err(io::Error::other("the rename was refused")) };
        let err = w.store.save_with(&key_1(), &w.snap(), &Steps { keep_frames: &real_frames, rename: &refuse }).unwrap_err();
        assert_eq!(err, "the recording could not be put into the folder for saved recordings.");
        assert_eq!(w.store.list(), vec![]);
        assert_eq!(tree(w.store.root()), Vec::<String>::new(), "not even the folders made for it are left");
    }

    #[test]
    fn a_folder_without_its_project_file_is_not_a_project() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        // A save cut off before its last step, or a project half removed by hand.
        std::fs::remove_file(dir.join(PROJECT_FILE)).unwrap();
        assert_eq!(w.store.list(), vec![]);
        assert_eq!(w.store.for_tour(SITE_A, TOUR_1), vec![]);
        assert_eq!(w.store.totals(), Totals::default());
        assert_eq!(load_err(&w.store, &key_1()), LoadError::NotSaved);
        assert!(dir.join(FRAMES_FILE).is_file(), "looking at it removes nothing");
        // Saving the tour again makes it a project again.
        w.save(&key_1());
        assert_eq!(keys(&w.store.list()), vec![key_1()]);
        assert!(tree(w.store.root()).iter().all(|n| !n.contains(STAGING)));
        loaded(&w.store, &key_1());
    }

    /// The state an editor sends with Update: another trim, no logo, the pasted picture replaced.
    fn edited(w: &World) -> (Edit, Vec<Item>) {
        let mut edit = w.edit.clone();
        edit.trim = (2, 5);
        edit.tour_doc.logo_path = None;
        let mut items: Vec<Item> = w.items.iter().filter(|it| !matches!(it.shape, mythicshot_editor::Shape::Image { .. })).cloned().collect();
        items.push(image_item(99, plain([1, 2, 3, 255])));
        (edit, items)
    }

    #[test]
    fn saving_again_replaces_the_state_and_keeps_the_frames_file() {
        let w = World::new();
        let first = w.save(&key_1());
        let dir = w.dir(&key_1());
        let old_picture = picture_names(&dir);
        let frames_written = std::fs::metadata(dir.join(FRAMES_FILE)).unwrap().modified().unwrap();
        // The editor that opened the project sends an update.
        let l = loaded(&w.store, &key_1());
        let (edit, items) = edited(&w);
        // Left behind by a save that was cut off, and a note somebody put into the folder.
        for stale in ["frames.zst.4242-17.tmp", "recording.json.4242-18.tmp", "project.json.0123456789abcdef.tmp", "pictures/00112233aabbccdd.png.0123456789abcdef.tmp"] {
            std::fs::write(dir.join(stale), b"cut off").unwrap();
        }
        std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
        let before = changes(&w.store);
        let snap = Snapshot { rec: &l.rec, title: "Chapter one, again", tour_title: "Customer onboarding", steps: 2, edit: &edit, events: &l.restored.events, items: &items };
        let second = w.store.save(&key_1(), &snap).unwrap();
        assert_eq!(changes(&w.store), before + 1);
        assert_eq!((second.info.title.as_str(), second.info.problem, second.info.dir.clone()), ("Chapter one, again", None, first.info.dir.clone()));
        assert!(second.info.saved_at >= first.info.saved_at);

        // The frames file was not written again; the old picture and the logo are gone, the new picture is there.
        assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), frames_and_index(FRAMES).0);
        assert_eq!(std::fs::metadata(dir.join(FRAMES_FILE)).unwrap().modified().unwrap(), frames_written);
        let new_picture = picture_names(&dir);
        assert_eq!(new_picture.len(), 1);
        assert_ne!(new_picture, old_picture);
        let mut want = vec![FRAMES_FILE.to_string(), "notes.txt".to_string(), "pictures/".to_string(), format!("pictures/{}", new_picture[0]), PROJECT_FILE.to_string(), INDEX_FILE.to_string()];
        want.sort();
        assert_eq!(tree(&dir), want, "what the project no longer names is removed, and nothing else");
        assert!(tree(w.store.root()).iter().all(|n| !n.contains(STAGING)));

        drop(l);
        let again = loaded(&w.store, &key_1());
        assert_same_edit(&again.restored.edit, &edit);
        assert_same_items(&again.restored.items, &items.iter().enumerate().map(|(i, it)| Item { id: i as u64 + 1, ..it.clone() }).collect::<Vec<_>>());
        assert_eq!((again.restored.title.as_str(), again.restored.dropped.len()), ("Chapter one, again", 0));
    }

    #[test]
    fn a_second_save_that_fails_leaves_the_first_one_whole() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        // A picture file the project does not name, which this save does not write either.
        std::fs::write(dir.join(PICTURES).join("00112233aabbccdd.png"), b"not this save's").unwrap();
        let whole = contents(&dir);
        let l = loaded(&w.store, &key_1());
        let (edit, items) = edited(&w);
        let snap = Snapshot { rec: &l.rec, title: "Chapter one, again", tour_title: "Customer onboarding", steps: 2, edit: &edit, events: &l.restored.events, items: &items };
        // Everything is written, and then the new project file cannot take the old one's place.
        let refuse = |_: &Path, to: &Path| -> io::Result<()> {
            assert!(to.ends_with(PROJECT_FILE));
            Err(io::Error::other("the rename was refused"))
        };
        let err = w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &real_frames, rename: &refuse }).unwrap_err();
        assert_eq!(err, "the project file could not be written.");
        assert_eq!(contents(&dir), whole, "the files this save made are gone, and no other file is touched");
        assert_eq!(changes(&w.store), 1);
        drop(l);
        let again = loaded(&w.store, &key_1());
        let mut want = w.edit.clone();
        want.tour_doc.logo_path = Some(dir.join(logo_name()));
        assert_same_edit(&again.restored.edit, &want);
        assert_same_items(&again.restored.items, &w.items);
        assert_eq!((again.restored.title.as_str(), again.restored.dropped.len()), ("Chapter one", 0));
    }

    #[test]
    fn saving_again_from_another_recording_replaces_the_whole_project_or_none_of_it() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
        let whole = contents(&dir);
        // The same tour and chapter, saved from a recording that is not the project's own.
        let other = recorded(w.tmp.path(), "other", 4);
        let edit = Edit::new(&other, Default::default());
        let snap = Snapshot { rec: &other, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] };
        let refuse = |_: &Path, _: &Path| -> io::Result<()> { Err(io::Error::other("the rename was refused")) };
        w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &real_frames, rename: &refuse }).unwrap_err();
        assert_eq!(contents(&dir), whole);
        // The old one moves out of the way, the new one cannot move in, and the old one is put back.
        let no_way_in = |from: &Path, to: &Path| if named(from, STAGING) { Err(io::Error::other("the rename was refused")) } else { std::fs::rename(from, to) };
        let err = w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &real_frames, rename: &no_way_in }).unwrap_err();
        assert_eq!(err, "the recording could not be put into the folder for saved recordings.");
        assert_eq!(contents(&dir), whole);
        assert_eq!((in_root(&w.store, STAGING), in_root(&w.store, REPLACED)), (vec![], vec![]));
        assert_eq!(changes(&w.store), 1);
        assert_eq!(loaded(&w.store, &key_1()).restored.title, "Chapter one");

        let saved = w.store.save(&key_1(), &snap).unwrap();
        assert_eq!((saved.info.title.as_str(), saved.info.problem), ("Shorter", None));
        assert_eq!(tree(&dir), vec![FRAMES_FILE.to_string(), "notes.txt".to_string(), PROJECT_FILE.to_string(), INDEX_FILE.to_string()], "the old project's files are gone, and a file that was nobody's is kept");
        assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), frames_and_index(4).0);
        assert_eq!((in_root(&w.store, STAGING), in_root(&w.store, REPLACED)), (vec![], vec![]));
        assert_eq!(loaded(&w.store, &key_1()).rec.frames.len(), 4);
    }

    #[test]
    fn a_project_opened_while_a_save_over_it_is_under_way_is_not_replaced() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        let whole = contents(&dir);
        let other = recorded(w.tmp.path(), "other", 4);
        let edit = Edit::new(&other, Default::default());
        let snap = Snapshot { rec: &other, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] };
        // While the frames are being kept, an editor opens the project.
        let opened = std::cell::RefCell::new(None);
        let slow = |rec: &Recording, to: &Path| {
            *opened.borrow_mut() = Some(loaded(&w.store, &key_1()));
            rec.save_into_with(to, &|| true)
        };
        let err = w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &slow, rename: &real_rename }).unwrap_err();
        assert!(err.contains("open in an editor"), "{err}");
        assert_eq!(contents(&dir), whole);
        assert_eq!(tree(w.store.root()).iter().filter(|n| n.contains(STAGING)).count(), 0);
        assert_eq!(opened.borrow().as_ref().unwrap().restored.title, "Chapter one");
    }

    #[test]
    fn a_frames_file_that_changed_under_an_open_project_stops_the_save() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        let l = loaded(&w.store, &key_1());
        let whole = contents(&dir);
        let frames = frames_and_index(FRAMES).0;
        assert!(frames_are(&dir, frames.len() as u64));
        assert!(!frames_are(&dir, frames.len() as u64 + 1));
        std::fs::remove_file(dir.join(FRAMES_FILE)).unwrap();
        assert!(!frames_are(&dir, frames.len() as u64));
        // A save from the open editor is refused, and writes nothing.
        let snap = Snapshot { rec: &l.rec, title: "Late", tour_title: "Customer onboarding", steps: 3, edit: &w.edit, events: &w.events, items: &w.items };
        let err = w.store.save(&key_1(), &snap).unwrap_err();
        assert!(err.ends_with('.'), "{err}");
        assert_eq!(contents(&dir), whole.into_iter().filter(|(name, _)| name != FRAMES_FILE).collect::<Vec<_>>());
    }

    #[test]
    fn an_open_project_is_never_saved_over_from_another_recording() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        let whole = contents(&dir);
        let l = loaded(&w.store, &key_1());
        let other = recorded(w.tmp.path(), "other", 4);
        let edit = Edit::new(&other, Default::default());
        let snap = Snapshot { rec: &other, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] };
        let err = w.store.save(&key_1(), &snap).unwrap_err();
        assert!(err.ends_with('.'), "{err}");
        assert_eq!(contents(&dir), whole);
        // As many frames at the same times, with no pointer, and still not the same frames.
        let alike = recorded_of(w.tmp.path(), "alike", FRAMES, 3);
        assert_eq!(alike.frames.iter().map(|m| (m.t_ms, m.cursor)).collect::<Vec<_>>(), l.rec.frames.iter().map(|m| (m.t_ms, m.cursor)).collect::<Vec<_>>());
        let snap = Snapshot { rec: &alike, title: "Alike", tour_title: "Customer onboarding", steps: 3, edit: &w.edit, events: &w.events, items: &w.items };
        let err = w.store.save(&key_1(), &snap).unwrap_err();
        assert!(err.contains("another recording"), "{err}");
        assert_eq!(contents(&dir), whole);
        drop(l);
    }

    #[test]
    fn a_picture_file_cut_short_by_an_earlier_save_is_written_again() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        let name = format!("pictures/{}", picture_names(&dir)[0]);
        let whole = std::fs::read(dir.join(&name)).unwrap();
        let l = loaded(&w.store, &key_1());
        std::fs::write(dir.join(&name), &whole[..whole.len() / 2]).unwrap();
        std::fs::write(dir.join(logo_name()), b"").unwrap();
        // The same state is saved again: the files have the right names, and not the right sizes.
        let edit = w.edit.clone();
        let snap = Snapshot { rec: &l.rec, title: "Chapter one", tour_title: "Customer onboarding", steps: 3, edit: &edit, events: &w.events, items: &w.items };
        let saved = w.store.save(&key_1(), &snap).unwrap();
        assert_eq!(saved.left_out, Vec::<String>::new());
        assert_eq!(std::fs::read(dir.join(&name)).unwrap(), whole);
        assert_eq!(std::fs::read(dir.join(logo_name())).unwrap().len(), std::fs::read(w.edit.tour_doc.logo_path.as_ref().unwrap()).unwrap().len());
        assert!(tree(&dir).iter().all(|n| !n.ends_with(".tmp")), "{:?}", tree(&dir));
    }

    #[test]
    fn what_could_not_be_saved_is_handed_to_the_caller() {
        let mut w = World::new();
        w.edit.tour_doc.logo_path = Some(w.tmp.path().join("no such logo.png"));
        let saved = w.save(&key_1());
        assert_eq!(saved.left_out, vec!["the cover logo".to_string()]);
        assert_eq!(saved.info.problem, None);
        let l = loaded(&w.store, &key_1());
        assert_eq!(l.restored.edit.tour_doc.logo_path, None);
        assert_eq!(l.restored.dropped, Vec::<String>::new());
    }

    #[test]
    fn a_project_is_found_only_for_its_own_address_and_tour() {
        let w = World::new();
        w.save(&key(SITE_A, TOUR_1, CH_1));
        w.save(&key(SITE_B, TOUR_1, CH_2));
        assert_eq!(keys(&w.store.for_tour(SITE_A, TOUR_1)), vec![key(SITE_A, TOUR_1, CH_1)]);
        assert_eq!(keys(&w.store.for_tour(SITE_B, TOUR_1)), vec![key(SITE_B, TOUR_1, CH_2)]);
        assert_eq!(w.store.for_tour(SITE_A, TOUR_2), vec![]);
        assert_eq!(w.store.for_tour("https://tours.example.com/", TOUR_1), vec![], "the address is matched exactly");
        // A whole site in one walk gives the same rows as asking tour by tour.
        w.save(&key(SITE_A, TOUR_2, CH_1));
        let site = w.store.for_site(SITE_A);
        assert_eq!(keys(&site), vec![key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_2, CH_1)]);
        assert_eq!(site, [w.store.for_tour(SITE_A, TOUR_1), w.store.for_tour(SITE_A, TOUR_2)].concat());
        assert_eq!(w.store.for_site("https://tours.example.com/"), vec![]);
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_2), Ok(1));
        assert_eq!(w.store.for_tour("https://third.example.com", TOUR_1), vec![]);
        assert_eq!(load_err(&w.store, &key(SITE_A, TOUR_1, CH_2)), LoadError::NotSaved);
        assert_eq!(load_err(&w.store, &key(SITE_B, TOUR_1, CH_1)), LoadError::NotSaved);
        assert_eq!(w.store.list().len(), 2);
    }

    #[test]
    fn two_chapters_of_one_tour_are_two_projects() {
        let mut w = World::new();
        w.save(&key(SITE_A, TOUR_1, CH_1));
        w.edit.trim = (0, 3);
        let second = w.store.save(&key(SITE_A, TOUR_1, CH_2), &Snapshot { title: "Chapter two", ..w.snap() }).unwrap();
        assert_eq!(second.info.title, "Chapter two");
        let both = w.store.for_tour(SITE_A, TOUR_1);
        assert_eq!(keys(&both), vec![key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2)]);
        assert_eq!(both.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(), vec!["Chapter one", "Chapter two"]);
        assert_ne!(both[0].dir, both[1].dir);
        // Both can be open at once, each with its own state.
        let (one, two) = (loaded(&w.store, &key(SITE_A, TOUR_1, CH_1)), loaded(&w.store, &key(SITE_A, TOUR_1, CH_2)));
        assert_eq!((one.restored.edit.trim, two.restored.edit.trim), ((1, 8), (0, 3)));
    }

    #[test]
    fn a_frames_file_deleted_or_cut_short_by_hand_is_a_problem_not_a_crash() {
        let w = World::new();
        w.save(&key(SITE_A, TOUR_1, CH_1));
        w.save(&key(SITE_A, TOUR_1, CH_2));
        w.save(&key(SITE_A, TOUR_2, CH_1));
        let (deleted, cut, no_index) = (w.dir(&key(SITE_A, TOUR_1, CH_1)), w.dir(&key(SITE_A, TOUR_1, CH_2)), w.dir(&key(SITE_A, TOUR_2, CH_1)));
        std::fs::remove_file(deleted.join(FRAMES_FILE)).unwrap();
        let frames = frames_and_index(FRAMES).0;
        std::fs::write(cut.join(FRAMES_FILE), &frames[..frames.len() - 7]).unwrap();
        std::fs::remove_file(no_index.join(INDEX_FILE)).unwrap();
        let before: Vec<_> = [&deleted, &cut, &no_index].iter().map(|d| contents(d)).collect();

        let list = w.store.list();
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|p| p.problem == Some(Problem::FramesMissing)), "{list:?}");
        assert!(list.iter().all(|p| p.title == "Chapter one"), "the rest of the row is still known");
        for k in keys(&list) {
            assert!(matches!(load_err(&w.store, &k), LoadError::Damaged(_)));
        }
        // A frames file of the right length that the index does not describe.
        let unlike = w.dir(&key(SITE_A, TOUR_2, CH_1));
        std::fs::write(unlike.join(INDEX_FILE), frames_and_index(FRAMES - 1).1).unwrap();
        assert!(matches!(load_err(&w.store, &key(SITE_A, TOUR_2, CH_1)), LoadError::Damaged(_)));
        std::fs::remove_file(unlike.join(INDEX_FILE)).unwrap();
        assert_eq!([&deleted, &cut, &no_index].iter().map(|d| contents(d)).collect::<Vec<_>>(), before, "nothing is repaired or removed");
        // A project that failed to open is not left registered as open.
        assert!(matches!(load_err(&w.store, &key(SITE_A, TOUR_1, CH_1)), LoadError::Damaged(_)));
    }

    #[test]
    fn a_project_from_a_newer_mythicshot_is_listed_with_its_problem_and_never_touched() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        let newer = serde_json::json!({"format": 3, "app": "9.0.0", "saved_at": 1_800_000_000_000i64, "server": SITE_A, "tour_id": TOUR_1, "chapter_id": CH_1, "tour_title": "Customer onboarding", "title": "From later", "edit": "something new"}).to_string();
        std::fs::write(dir.join(PROJECT_FILE), &newer).unwrap();
        let whole = contents(&dir);

        let list = w.store.for_tour(SITE_A, TOUR_1);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].problem, list[0].key.clone(), list[0].title.as_str(), list[0].tour_title.as_str(), list[0].saved_at), (Some(Problem::Newer), key_1(), "From later", "Customer onboarding", 1_800_000_000_000));
        assert_eq!(w.store.list(), list);
        assert_eq!(load_err(&w.store, &key_1()), LoadError::Newer);
        // This version does not save over it either.
        let err = w.store.save(&key_1(), &w.snap()).unwrap_err();
        assert!(err.contains("newer") && err.ends_with('.'), "{err}");
        assert_eq!(contents(&dir), whole);
        assert!(tree(w.store.root()).iter().all(|n| !n.contains(STAGING)));
    }

    #[test]
    fn a_damaged_project_file_is_listed_with_its_problem_and_never_touched() {
        let w = World::new();
        w.save(&key(SITE_A, TOUR_1, CH_1));
        w.save(&key(SITE_A, TOUR_1, CH_2));
        w.save(&key(SITE_A, TOUR_2, CH_1));
        // Cut off in the middle; not text at all; and a project folder copied under another tour.
        let (cut, binary, moved) = (w.dir(&key(SITE_A, TOUR_1, CH_1)), w.dir(&key(SITE_A, TOUR_1, CH_2)), w.dir(&key(SITE_A, TOUR_2, CH_1)));
        let text = std::fs::read(cut.join(PROJECT_FILE)).unwrap();
        std::fs::write(cut.join(PROJECT_FILE), &text[..text.len() / 2]).unwrap();
        std::fs::write(binary.join(PROJECT_FILE), [0xffu8, 0xfe, 0x00, 0x12]).unwrap();
        std::fs::write(moved.join(PROJECT_FILE), std::fs::read(&{ w.save(&key(SITE_B, TOUR_2, CH_2)).info.dir.join(PROJECT_FILE) }).unwrap()).unwrap();
        let before: Vec<_> = [&cut, &binary, &moved].iter().map(|d| contents(d)).collect();

        let list: Vec<ProjectInfo> = w.store.list().into_iter().filter(|p| p.key.server != SITE_B).collect();
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|p| p.problem == Some(Problem::Damaged)), "{list:?}");
        assert_eq!(keys(&w.store.for_tour(SITE_A, TOUR_1)), vec![key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2)], "the row still knows its address, tour and chapter");
        for k in [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1)] {
            assert!(matches!(load_err(&w.store, &k), LoadError::Damaged(_)));
        }
        assert_eq!([&cut, &binary, &moved].iter().map(|d| contents(d)).collect::<Vec<_>>(), before);
    }

    #[test]
    fn a_project_file_larger_than_the_limit_is_damaged_and_is_not_read() {
        let w = World::new();
        w.save(&key_1());
        let path = w.dir(&key_1()).join(PROJECT_FILE);
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(MAX_PROJECT_JSON_BYTES as u64 + 1).unwrap();
        assert_eq!(w.store.list()[0].problem, Some(Problem::Damaged));
        assert!(matches!(load_err(&w.store, &key_1()), LoadError::Damaged(_)));
    }

    #[test]
    fn a_project_cannot_be_opened_twice_and_can_again_after_it_is_closed() {
        let w = World::new();
        w.save(&key_1());
        w.save(&key(SITE_A, TOUR_1, CH_2));
        let first = loaded(&w.store, &key_1());
        assert_eq!(load_err(&w.store, &key_1()), LoadError::AlreadyOpen);
        // Through another store on the same folder too, and another chapter is not in the way.
        assert_eq!(load_err(&Store::at(w.store.root().to_path_buf()), &key_1()), LoadError::AlreadyOpen);
        loaded(&w.store, &key(SITE_A, TOUR_1, CH_2));
        let Loaded { rec, open, .. } = first;
        drop(rec);
        assert_eq!(load_err(&w.store, &key_1()), LoadError::AlreadyOpen, "the guard is what keeps it open");
        drop(open);
        loaded(&w.store, &key_1());
        assert_eq!(w.store.list().len(), 2, "closing a project removes nothing");
    }

    #[test]
    fn removing_a_tour_removes_its_projects_and_no_other_tours() {
        let w = World::new();
        for k in [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1), key(SITE_B, TOUR_1, CH_1)] {
            w.save(&k);
        }
        // Somebody's own file inside a project folder, and a project half removed earlier.
        std::fs::write(w.dir(&key(SITE_A, TOUR_1, CH_1)).join("notes.txt"), b"mine").unwrap();
        std::fs::remove_file(w.dir(&key(SITE_A, TOUR_1, CH_2)).join(PROJECT_FILE)).unwrap();
        let before = changes(&w.store);
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(1), "one project was there to remove");
        assert_eq!(changes(&w.store), before + 1);
        assert_eq!(keys(&w.store.list()), {
            let mut left = vec![key(SITE_A, TOUR_2, CH_1), key(SITE_B, TOUR_1, CH_1)];
            left.sort_by_key(|k| w.dir(k));
            left
        });
        let tour_dir = w.store.root().join(site_of(SITE_A)).join(TOUR_1);
        assert_eq!(tree(&tour_dir), vec![format!("{CH_1}/"), format!("{CH_1}/notes.txt")], "only what the store wrote is removed");
        assert_eq!(w.store.for_tour(SITE_A, TOUR_1), vec![]);
        loaded(&w.store, &key(SITE_A, TOUR_2, CH_1));
        loaded(&w.store, &key(SITE_B, TOUR_1, CH_1));
        // Again, and for a tour that never had one: nothing to do, and not an error.
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(0));
        assert_eq!(w.store.remove_tour("https://third.example.com", TOUR_2), Ok(0));
        // The last project of an address takes the emptied folders with it.
        assert_eq!(w.store.remove_tour(SITE_B, TOUR_1), Ok(1));
        assert!(!w.store.root().join(site_of(SITE_B)).exists());
        assert_eq!(w.store.list().len(), 1);
    }

    #[test]
    fn a_project_open_in_an_editor_is_removed_when_the_editor_closes() {
        let w = World::new();
        w.save(&key(SITE_A, TOUR_1, CH_1));
        w.save(&key(SITE_A, TOUR_1, CH_2));
        let open = loaded(&w.store, &key(SITE_A, TOUR_1, CH_1));
        let before = changes(&w.store);
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(2));
        assert_eq!(changes(&w.store), before + 1);
        // Neither is listed or counted any more; the open one's files are whole until its editor closes.
        assert_eq!(w.store.list(), vec![]);
        assert_eq!(w.store.for_tour(SITE_A, TOUR_1), vec![]);
        assert_eq!(w.store.totals(), Totals::default());
        assert_eq!(std::fs::read(open.info.dir.join(FRAMES_FILE)).unwrap(), frames_and_index(FRAMES).0);
        assert!(open.info.dir.join(PROJECT_FILE).is_file() && open.info.dir.join(REMOVING_FILE).is_file());
        assert_eq!(load_err(&w.store, &key(SITE_A, TOUR_1, CH_1)), LoadError::AlreadyOpen);
        // Removing it again counts nothing, and start-up housekeeping leaves an open project alone.
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(0));
        w.store.finish_removals();
        assert!(open.info.dir.join(FRAMES_FILE).is_file());
        let before = changes(&w.store);
        drop(open);
        assert_eq!(changes(&w.store), before + 1);
        assert_eq!(tree(w.store.root()), Vec::<String>::new());
        assert_eq!(load_err(&w.store, &key(SITE_A, TOUR_1, CH_1)), LoadError::NotSaved);
    }

    #[test]
    fn a_removal_cut_off_by_a_crash_is_finished_at_the_next_start() {
        let w = World::new();
        w.save(&key(SITE_A, TOUR_1, CH_1));
        w.save(&key(SITE_A, TOUR_2, CH_1));
        std::fs::write(w.dir(&key(SITE_A, TOUR_1, CH_1)).join("notes.txt"), b"mine").unwrap();
        let open = loaded(&w.store, &key(SITE_A, TOUR_1, CH_1));
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(1));
        // The app dies with the editor open: the guard never runs.
        forget_open(open);
        let dir = w.dir(&key(SITE_A, TOUR_1, CH_1));
        assert!(dir.join(PROJECT_FILE).is_file() && dir.join(FRAMES_FILE).is_file(), "the files are still there");
        // The delete the user asked for holds across the restart: not listed, not counted, not opened.
        assert_eq!(keys(&w.store.list()), vec![key(SITE_A, TOUR_2, CH_1)]);
        assert_eq!(w.store.totals().count, 1);
        assert_eq!(load_err(&w.store, &key(SITE_A, TOUR_1, CH_1)), LoadError::NotSaved);
        assert!(dir.join(FRAMES_FILE).is_file(), "looking at it removes nothing");
        let before = changes(&w.store);
        w.store.finish_removals();
        assert_eq!(changes(&w.store), before + 1);
        assert_eq!(tree(&w.store.root().join(site_of(SITE_A)).join(TOUR_1)), vec![format!("{CH_1}/"), format!("{CH_1}/notes.txt")], "what the store wrote is gone, and nothing else");
        assert_eq!(keys(&w.store.list()), vec![key(SITE_A, TOUR_2, CH_1)]);
        loaded(&w.store, &key(SITE_A, TOUR_2, CH_1));
        // Nothing left to finish: nothing changes.
        w.store.finish_removals();
        assert_eq!(changes(&w.store), before + 1);
    }

    #[cfg(windows)]
    #[test]
    fn a_removal_that_cannot_take_every_file_keeps_its_mark_and_is_finished_later() {
        use std::os::windows::fs::OpenOptionsExt;
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        // Another program has the frames file open and does not let it be deleted.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1 | 2).open(dir.join(FRAMES_FILE)).unwrap();
        let err = w.store.remove_tour(SITE_A, TOUR_1).unwrap_err();
        assert!(err.ends_with('.') && !err.contains(&*w.tmp.path().to_string_lossy()), "{err}");
        assert!(!dir.join(PROJECT_FILE).exists() && dir.join(REMOVING_FILE).is_file() && dir.join(FRAMES_FILE).is_file());
        assert_eq!(w.store.list(), vec![]);
        drop(held);
        w.store.finish_removals();
        assert_eq!(tree(w.store.root()), Vec::<String>::new());
    }

    #[test]
    fn totals_count_every_address() {
        let w = World::new();
        assert_eq!(w.store.totals(), Totals { count: 0, bytes: 0 });
        assert_eq!(Store::at(w.tmp.path().join("never made")).totals(), Totals::default());
        let sizes: Vec<u64> = [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_2, CH_1), key(SITE_B, TOUR_1, CH_1)].iter().map(|k| w.save(k).info.bytes).collect();
        assert!(sizes.iter().all(|b| *b > frames_and_index(FRAMES).0.len() as u64));
        assert_eq!(w.store.totals(), Totals { count: 3, bytes: sizes.iter().sum() });
        // A staging folder and a stray file are not projects.
        for not_a_site in [".staging-1763460000000-0123456789abcdef", ".replaced-0123456789abcdef"] {
            std::fs::create_dir_all(w.store.root().join(not_a_site).join(TOUR_1).join(CH_1)).unwrap();
            std::fs::copy(w.dir(&key(SITE_A, TOUR_1, CH_1)).join(PROJECT_FILE), w.store.root().join(not_a_site).join(TOUR_1).join(CH_1).join(PROJECT_FILE)).unwrap();
        }
        std::fs::write(w.store.root().join("readme.txt"), b"hello").unwrap();
        assert_eq!(w.store.totals().count, 3);
    }

    #[test]
    fn an_id_that_cannot_be_a_folder_name_is_refused() {
        let w = World::new();
        for (tour, chapter) in [("..", CH_1), ("../../outside", CH_1), (TOUR_1, "a/b"), (TOUR_1, "a\\b"), ("", CH_1), (TOUR_1, ""), ("con", CH_1), (TOUR_1, "C:"), ("Tour_1", CH_1), (&"a".repeat(65), CH_1)] {
            let k = key(SITE_A, tour, chapter);
            let err = w.store.save(&k, &w.snap()).unwrap_err();
            assert!(err.ends_with('.'), "{err}");
            assert_eq!(load_err(&w.store, &k), LoadError::NotSaved);
            assert_eq!(w.store.for_tour(SITE_A, tour), vec![]);
            assert_eq!(w.store.remove_tour(SITE_A, tour), Ok(0));
        }
        assert!(!w.store.root().exists(), "nothing was written anywhere");
        assert_eq!(tree(w.tmp.path()).iter().filter(|n| !n.starts_with("rec/")).collect::<Vec<_>>(), Vec::<&String>::new());
    }

    fn since_1970(t: SystemTime) -> u128 {
        t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_millis()
    }

    const DAY: Duration = Duration::from_secs(86_400);
    const HOUR: Duration = Duration::from_secs(3_600);

    #[test]
    fn only_old_staging_folders_with_the_exact_name_are_swept() {
        let w = World::new();
        w.save(&key_1());
        let root = w.store.root().to_path_buf();
        let now = SystemTime::now();
        let started = since_1970(now);
        let cut_off = root.join(format!(".staging-{started}-0123456789abcdef"));
        std::fs::create_dir_all(cut_off.join(PICTURES)).unwrap();
        std::fs::write(cut_off.join(FRAMES_FILE), b"half").unwrap();
        let not_ours = [
            ".staging-".to_string(),
            ".staging-0123456789abcdef".to_string(),
            format!(".staging-{started}-0123456789abcde"),
            format!(".staging-{started}-0123456789abcdef0"),
            format!(".staging-{started}-0A1B2C3D4E5F6A7B"),
            format!(".staging-{started}-0123456789abcdeg"),
            format!(".staging--{started}-1123456789abcdef"),
            format!(".staging-x{started}-2123456789abcdef"),
            format!(".staging-{started}0000-3123456789abcdef"),
            format!("staging-{started}-0123456789abcdef"),
            format!(".staging-{started}-0123456789abcdef.bak"),
            ".replaced-0123456789abcdef".to_string(),
            format!(".replaced-{started}-0123456789abcdef"),
        ];
        for name in &not_ours {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("kept.txt"), b"x").unwrap();
        }
        // A file with the exact name, and a folder with it one level down.
        std::fs::write(root.join(format!(".staging-{started}-fedcba9876543210")), b"a file").unwrap();
        std::fs::create_dir(root.join(site_of(SITE_A)).join(format!(".staging-{started}-0123456789abcdef"))).unwrap();
        let everything = tree(&root);

        // Not old enough yet: a save may still be writing it.
        w.store.sweep_staging(DAY);
        w.store.sweep_staging_at(DAY, now + 23 * HOUR);
        assert_eq!(tree(&root), everything);
        // The age is the one in the name: a folder made this moment whose name says two days ago goes.
        let long_ago = root.join(format!(".staging-{}-aaaaaaaaaaaaaaaa", started - 2 * DAY.as_millis()));
        std::fs::create_dir(&long_ago).unwrap();
        std::fs::write(long_ago.join(FRAMES_FILE), b"half").unwrap();
        w.store.sweep_staging(DAY);
        assert!(!long_ago.exists());
        assert_eq!(tree(&root), everything);
        // A day and an hour later the cut-off save goes, and only that.
        w.store.sweep_staging_at(DAY, now + 25 * HOUR);
        assert!(!cut_off.exists());
        let gone = format!(".staging-{started}-0123456789abcdef/");
        assert_eq!(tree(&root), everything.into_iter().filter(|n| !n.starts_with(&gone)).collect::<Vec<_>>());
        assert_eq!(changes(&w.store), 1, "sweeping is not a change to the list");
        assert_eq!(keys(&w.store.list()), vec![key_1()]);
        loaded(&w.store, &key_1());
        // A projects folder that does not exist is nothing to sweep.
        Store::at(w.tmp.path().join("never made")).sweep_staging(Duration::ZERO);
        assert!(!w.tmp.path().join("never made").exists());
    }

    #[test]
    fn a_save_in_progress_is_named_by_the_time_it_started() {
        let w = World::new();
        let seen = std::cell::RefCell::new(None);
        let look = |rec: &Recording, to: &Path| {
            *seen.borrow_mut() = to.file_name().and_then(|n| n.to_str()).map(str::to_string);
            rec.save_into_with(to, &|| true)
        };
        let before = since_1970(SystemTime::now());
        w.store.save_with(&key_1(), &w.snap(), &Steps { keep_frames: &look, rename: &real_rename }).unwrap();
        let name = seen.borrow().clone().unwrap();
        let started = u128::from(staging_started(&name).unwrap_or_else(|| panic!("{name} is not a name the sweep knows")));
        assert!((before..=since_1970(SystemTime::now())).contains(&started), "{name}");
    }

    fn copy_tree(from: &Path, to: &Path) {
        for (name, bytes) in contents(from) {
            let path = to.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }

    const FAR: Duration = Duration::from_secs(10 * 365 * 86_400);

    #[test]
    fn an_old_project_that_cannot_be_put_back_is_never_swept() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
        let whole = contents(&dir);
        let other = recorded(w.tmp.path(), "other", 4);
        let edit = Edit::new(&other, Default::default());
        let snap = Snapshot { rec: &other, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] };
        // The old project moves out of the way, and then nothing can be moved into its place:
        // not the new one, and not the old one back.
        let stuck = |from: &Path, to: &Path| if to == dir { Err(io::Error::other("the rename was refused")) } else { std::fs::rename(from, to) };
        let err = w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &real_frames, rename: &stuck }).unwrap_err();
        assert_eq!(err, NOT_PUT_BACK);
        assert!(!dir.exists());
        let aside = in_root(&w.store, REPLACED);
        assert_eq!(aside.len(), 1);
        assert_eq!(contents(&aside[0]), whole, "the only copy is whole, under a name of its own");
        assert_eq!(in_root(&w.store, STAGING), Vec::<PathBuf>::new());
        assert_eq!((w.store.list(), w.store.totals()), (vec![], Totals::default()));
        assert_eq!(load_err(&w.store, &key_1()), LoadError::NotSaved);
        // No sweep takes it, however old it is said to be.
        w.store.sweep_staging(Duration::ZERO);
        w.store.sweep_staging_at(Duration::ZERO, SystemTime::now() + FAR);
        w.store.finish_removals();
        assert_eq!(contents(&aside[0]), whole);

        // The next start puts it back where it was.
        let before = changes(&w.store);
        w.store.recover_replaced();
        assert_eq!(changes(&w.store), before + 1);
        assert_eq!(contents(&dir), whole);
        assert_eq!(in_root(&w.store, REPLACED), Vec::<PathBuf>::new());
        assert_eq!(keys(&w.store.list()), vec![key_1()]);
        assert_eq!(loaded(&w.store, &key_1()).restored.title, "Chapter one");
        w.store.recover_replaced();
        assert_eq!(changes(&w.store), before + 1);
    }

    #[test]
    fn at_the_next_start_what_a_replaced_project_left_is_cleared_and_nothing_else() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        let root = w.store.root().to_path_buf();
        // The app died after the new project was in place and before the old one was cleared away.
        let aside = root.join(".replaced-0123456789abcdef");
        copy_tree(&dir, &aside);
        std::fs::write(aside.join("notes.txt"), b"mine").unwrap();
        std::fs::write(aside.join(PICTURES).join("mine.txt"), b"mine too").unwrap();
        // Replaced folders the store cannot tell the place of, and a folder that only looks like one.
        let (bare, broken, misnamed) = (root.join(".replaced-1111111111111111"), root.join(".replaced-2222222222222222"), root.join(".replaced-xyz"));
        copy_tree(&dir, &bare);
        std::fs::remove_file(bare.join(PROJECT_FILE)).unwrap();
        copy_tree(&dir, &broken);
        std::fs::write(broken.join(PROJECT_FILE), b"{\"format\": 1, \"server\"").unwrap();
        copy_tree(&dir, &misnamed);
        let whole = contents(&dir);
        let left_alone: Vec<_> = [&bare, &broken, &misnamed].iter().map(|d| contents(d)).collect();

        w.store.recover_replaced();
        // The project in place is as it was; the note moved along with it, since its name was free.
        let mut want = whole.clone();
        want.push(("notes.txt".to_string(), b"mine".to_vec()));
        want.sort();
        assert_eq!(contents(&dir), want);
        assert_eq!(contents(&aside), vec![("pictures/mine.txt".to_string(), b"mine too".to_vec())], "the store's files are gone; a file that is not the store's stays");
        assert_eq!([&bare, &broken, &misnamed].iter().map(|d| contents(d)).collect::<Vec<_>>(), left_alone);
        assert_eq!(changes(&w.store), 1, "the list did not change");
        // None of it is ever swept.
        w.store.finish_removals();
        w.store.sweep_staging_at(Duration::ZERO, SystemTime::now() + FAR);
        assert_eq!(contents(&aside), vec![("pictures/mine.txt".to_string(), b"mine too".to_vec())]);
        assert_eq!([&bare, &broken, &misnamed].iter().map(|d| contents(d)).collect::<Vec<_>>(), left_alone);
        assert_eq!(keys(&w.store.list()), vec![key_1()]);
    }

    #[test]
    fn a_project_in_place_that_cannot_be_opened_keeps_the_replaced_copy_whole() {
        let w = World::new();
        for k in [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1)] {
            w.save(&k);
        }
        let root = w.store.root().to_path_buf();
        let (cut, no_frames, gone) = (w.dir(&key(SITE_A, TOUR_1, CH_1)), w.dir(&key(SITE_A, TOUR_1, CH_2)), w.dir(&key(SITE_A, TOUR_2, CH_1)));
        let asides = [root.join(".replaced-1111111111111111"), root.join(".replaced-2222222222222222"), root.join(".replaced-3333333333333333")];
        for (dir, aside) in [&cut, &no_frames, &gone].iter().zip(&asides) {
            copy_tree(dir, aside);
        }
        // What is in place: a project file cut off, a project without its frames, and one being removed.
        let text = std::fs::read(cut.join(PROJECT_FILE)).unwrap();
        std::fs::write(cut.join(PROJECT_FILE), &text[..text.len() / 2]).unwrap();
        std::fs::remove_file(no_frames.join(FRAMES_FILE)).unwrap();
        std::fs::write(gone.join(REMOVING_FILE), b"").unwrap();
        let whole: Vec<_> = asides.iter().map(|d| contents(d)).collect();
        let in_place: Vec<_> = [&cut, &no_frames, &gone].iter().map(|d| contents(d)).collect();

        w.store.recover_replaced();
        assert_eq!(asides.iter().map(|d| contents(d)).collect::<Vec<_>>(), whole, "the copy that can still be opened is not the one to go");
        assert_eq!([&cut, &no_frames, &gone].iter().map(|d| contents(d)).collect::<Vec<_>>(), in_place);
        assert_eq!(changes(&w.store), 3);
    }

    /// The folder's project file with its time of saving moved by `by` milliseconds.
    fn saved_later(dir: &Path, by: i64) {
        let mut file = state::parse(&std::fs::read_to_string(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        file.saved_at += by;
        std::fs::write(dir.join(PROJECT_FILE), state::to_text(&file).unwrap()).unwrap();
    }

    #[test]
    fn of_two_replaced_copies_only_one_no_newer_than_the_project_in_place_is_cleared() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        let root = w.store.root().to_path_buf();
        let (older, same, newer) = (root.join(".replaced-1111111111111111"), root.join(".replaced-2222222222222222"), root.join(".replaced-3333333333333333"));
        for aside in [&older, &same, &newer] {
            copy_tree(&dir, aside);
        }
        saved_later(&older, -60_000);
        saved_later(&newer, 60_000);
        let (whole, kept) = (contents(&dir), contents(&newer));

        w.store.recover_replaced();
        assert!(!older.exists() && !same.exists(), "the project in place is at least as new as these");
        assert_eq!(contents(&newer), kept, "a copy saved after the project in place is not cleared away");
        assert_eq!(contents(&dir), whole);
        assert_eq!(keys(&w.store.list()), vec![key_1()]);
    }

    #[test]
    fn a_replaced_folder_whose_clearing_was_cut_off_is_cleared_at_the_next_start() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        let whole = contents(&dir);
        // The app died while the old project's files were being removed: the mark, the files not
        // yet reached, and a note that was never the store's.
        let aside = w.store.root().join(".replaced-0123456789abcdef");
        copy_tree(&dir, &aside);
        std::fs::remove_file(aside.join(PROJECT_FILE)).unwrap();
        std::fs::write(aside.join(REMOVING_FILE), b"").unwrap();
        std::fs::write(aside.join("notes.txt"), b"mine").unwrap();
        // One with nothing but the store's files left goes whole.
        let bare = w.store.root().join(".replaced-1111111111111111");
        copy_tree(&dir, &bare);
        std::fs::remove_file(bare.join(PROJECT_FILE)).unwrap();
        std::fs::write(bare.join(REMOVING_FILE), b"").unwrap();

        w.store.recover_replaced();
        assert_eq!(contents(&aside), vec![("notes.txt".to_string(), b"mine".to_vec())]);
        assert!(!bare.exists());
        assert_eq!(contents(&dir), whole);
        assert_eq!(changes(&w.store), 1);
    }

    #[test]
    fn a_file_somebody_put_into_a_replaced_folder_is_never_deleted() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        // The project had a note beside it, was replaced whole, and the new one got a note of the same name.
        std::fs::write(dir.join("notes.txt"), b"old note").unwrap();
        let other = recorded(w.tmp.path(), "other", 4);
        let edit = Edit::new(&other, Default::default());
        let snap = Snapshot { rec: &other, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] };
        // The new project arrives with a file of that name already in it.
        let with_a_note = |rec: &Recording, to: &Path| {
            std::fs::write(to.join("notes.txt"), b"new note").unwrap();
            rec.save_into_with(to, &|| true)
        };
        w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &with_a_note, rename: &real_rename }).unwrap();
        assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"new note");
        let aside = in_root(&w.store, REPLACED);
        assert_eq!(aside.len(), 1);
        assert_eq!(contents(&aside[0]), vec![("notes.txt".to_string(), b"old note".to_vec())], "only the note is left of the old project, and it is kept");
        // Start-up housekeeping, any number of times, leaves it.
        for _ in 0..2 {
            w.store.recover_replaced();
            w.store.finish_removals();
            w.store.sweep_staging_at(Duration::ZERO, SystemTime::now() + FAR);
        }
        assert_eq!(contents(&aside[0]), vec![("notes.txt".to_string(), b"old note".to_vec())]);
        assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"new note");
        assert_eq!(loaded(&w.store, &key_1()).restored.title, "Shorter");
    }

    #[cfg(windows)]
    #[test]
    fn a_pictures_folder_that_is_a_link_is_never_read_or_emptied() {
        let w = World::new();
        w.save(&key_1());
        let dir = w.dir(&key_1());
        // The pictures folder is moved elsewhere and a link to it is left in its place.
        let outside = w.tmp.path().join("outside");
        std::fs::rename(dir.join(PICTURES), &outside).unwrap();
        let link = dir.join(PICTURES);
        let linked = std::os::windows::fs::symlink_dir(&outside, &link).is_ok() || std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&outside).output().is_ok_and(|o| o.status.success());
        assert!(linked, "neither a folder link nor a junction could be made on this computer");
        assert!(link.join(&picture_names(&dir)[0]).is_file(), "the link leads to the picture");
        let kept = contents(&outside);
        assert_eq!(kept.len(), 1);

        let top: u64 = files_in(&dir).iter().map(|(_, path)| std::fs::metadata(path).unwrap().len()).sum();
        assert_eq!(w.store.list()[0].bytes, top, "what the link leads to is not counted");
        assert!(matches!(load_err(&w.store, &key_1()), LoadError::Damaged(_)));
        // A save over it does not write through the link, and a removal does not reach through it.
        let other = recorded(w.tmp.path(), "other", 4);
        let edit = Edit::new(&other, Default::default());
        w.store.save(&key_1(), &Snapshot { rec: &other, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] }).unwrap();
        assert_eq!(contents(&outside), kept);
        // The link stays behind with the replaced project; the new project has none and opens.
        assert!(std::fs::symlink_metadata(dir.join(PICTURES)).is_err(), "the link was not moved into the new project");
        let aside = in_root(&w.store, REPLACED);
        assert_eq!(aside.len(), 1);
        assert!(std::fs::symlink_metadata(aside[0].join(PICTURES)).is_ok());
        assert_eq!(loaded(&w.store, &key_1()).restored.title, "Shorter");
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(1));
        assert_eq!(contents(&outside), kept, "the picture behind the link is still there");
        for _ in 0..2 {
            w.store.recover_replaced();
            w.store.finish_removals();
        }
        assert_eq!(contents(&outside), kept);
    }

    /// [`Store::remove_project`]: one chapter's project, by its folder.
    mod one_project {
        use super::*;

        /// The folder behind a link: a folder link where the system allows one, else a junction.
        #[cfg(windows)]
        fn link_to(outside: &Path, link: &Path) {
            let linked = std::os::windows::fs::symlink_dir(outside, link).is_ok() || std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(outside).output().is_ok_and(|o| o.status.success());
            assert!(linked, "neither a folder link nor a junction could be made on this computer");
        }

        #[test]
        fn removing_one_project_leaves_the_other_chapters_and_tours_alone() {
            let w = World::new();
            let (gone, sibling, other_tour, other_site) = (key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1), key(SITE_B, TOUR_1, CH_1));
            for k in [&gone, &sibling, &other_tour, &other_site] {
                w.save(k);
            }
            let kept = |w: &World| [&sibling, &other_tour, &other_site].map(|k| contents(&w.dir(k)));
            let before = kept(&w);
            assert_eq!(w.store.remove_project(&w.dir(&gone)), Ok(Removed::Now));
            assert!(!w.dir(&gone).exists(), "the project's folder went with its files");
            assert_eq!(kept(&w), before, "no other project lost or gained a byte");
            assert_eq!(keys(&w.store.list()), {
                let mut left = vec![sibling.clone(), other_tour.clone(), other_site.clone()];
                left.sort_by_key(|k| w.dir(k));
                left
            });
            for k in [&sibling, &other_tour, &other_site] {
                loaded(&w.store, k);
            }
            // The tour's folder goes with its last chapter, the site's with its last tour, and no sooner.
            let site_a = w.store.root().join(site_of(SITE_A));
            assert_eq!(w.store.remove_project(&w.dir(&sibling)), Ok(Removed::Now));
            assert!(!site_a.join(TOUR_1).exists() && site_a.join(TOUR_2).is_dir());
            assert_eq!(w.store.remove_project(&w.dir(&other_site)), Ok(Removed::Now));
            assert!(!w.store.root().join(site_of(SITE_B)).exists());
            assert_eq!(keys(&w.store.list()), vec![other_tour.clone()]);
            assert_eq!(kept(&w)[1], before[1]);
        }

        #[test]
        fn a_project_open_in_an_editor_is_removed_when_the_editor_closes() {
            let w = World::new();
            let (open_key, sibling) = (key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2));
            w.save(&open_key);
            let sibling_info = w.save(&sibling).info;
            let open = loaded(&w.store, &open_key);
            let dir = open.info.dir.clone();
            let whole = contents(&dir);
            let before = changes(&w.store);
            assert_eq!(w.store.remove_project(&dir), Ok(Removed::WhenItsEditorCloses));
            assert_eq!(changes(&w.store), before + 1);
            // Not listed or counted any more; its files are whole, with the mark, until its editor closes.
            assert_eq!(w.store.list(), vec![sibling_info.clone()]);
            assert_eq!(w.store.totals().count, 1);
            assert!(dir.join(REMOVING_FILE).is_file());
            assert_eq!(contents(&dir).into_iter().filter(|(name, _)| name != REMOVING_FILE).collect::<Vec<_>>(), whole);
            assert_eq!(load_err(&w.store, &open_key), LoadError::AlreadyOpen);
            // Asked again, the answer is the same and nothing more happens; housekeeping leaves it too.
            let marked = changes(&w.store);
            assert_eq!(w.store.remove_project(&dir), Ok(Removed::WhenItsEditorCloses));
            assert_eq!(changes(&w.store), marked, "nothing changed, so no page reads the list again");
            w.store.finish_removals();
            assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), frames_and_index(FRAMES).0);
            let before = changes(&w.store);
            drop(open);
            assert_eq!(changes(&w.store), before + 1);
            assert_eq!(tree(&w.store.root().join(site_of(SITE_A)).join(TOUR_1)).into_iter().filter(|n| !n.starts_with(CH_2)).collect::<Vec<_>>(), Vec::<String>::new(), "the open project is gone, the other chapter is not");
            assert_eq!(load_err(&w.store, &open_key), LoadError::NotSaved);
            assert_eq!(w.store.list(), vec![sibling_info]);
            loaded(&w.store, &sibling);
        }

        #[test]
        fn a_removal_cut_off_by_a_crash_is_finished_at_the_next_start() {
            let w = World::new();
            let (first, second, stays) = (key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1));
            for k in [&first, &second, &stays] {
                w.save(k);
            }
            std::fs::write(w.dir(&first).join("notes.txt"), b"mine").unwrap();
            // The app dies with the editor of a removed project open: the guard never runs.
            let open = loaded(&w.store, &first);
            assert_eq!(w.store.remove_project(&w.dir(&first)), Ok(Removed::WhenItsEditorCloses));
            forget_open(open);
            // The app dies in the middle of a removal: the mark is written and the project file is gone.
            mark_removing(&w.dir(&second)).unwrap();
            std::fs::remove_file(w.dir(&second).join(PROJECT_FILE)).unwrap();
            assert!(w.dir(&first).join(FRAMES_FILE).is_file() && w.dir(&second).join(FRAMES_FILE).is_file(), "the files are still there");
            // The removal the user asked for holds across the restart: not listed, not counted, not opened.
            assert_eq!(keys(&w.store.list()), vec![stays.clone()]);
            assert_eq!(w.store.totals().count, 1);
            assert_eq!(load_err(&w.store, &first), LoadError::NotSaved);
            w.store.finish_removals();
            assert_eq!(tree(&w.store.root().join(site_of(SITE_A)).join(TOUR_1)), vec![format!("{CH_1}/"), format!("{CH_1}/notes.txt")], "what the store wrote is gone, and nothing else");
            assert_eq!(keys(&w.store.list()), vec![stays.clone()]);
            loaded(&w.store, &stays);
        }

        #[cfg(windows)]
        #[test]
        fn a_removal_that_cannot_take_every_file_says_so_and_is_finished_later() {
            use std::os::windows::fs::OpenOptionsExt;
            let w = World::new();
            w.save(&key_1());
            let dir = w.dir(&key_1());
            // Another program has the frames file open and does not let it be deleted.
            let held = std::fs::OpenOptions::new().read(true).share_mode(1 | 2).open(dir.join(FRAMES_FILE)).unwrap();
            let before = changes(&w.store);
            let err = w.store.remove_project(&dir).unwrap_err();
            assert_eq!(err, "this saved recording could not be removed.");
            assert!(reads_as_a_reason(&err, w.tmp.path()));
            assert_eq!(changes(&w.store), before + 1, "it is no longer listed, so the pages read the list again");
            assert!(!dir.join(PROJECT_FILE).exists() && dir.join(REMOVING_FILE).is_file() && dir.join(FRAMES_FILE).is_file());
            assert_eq!(w.store.list(), vec![]);
            drop(held);
            // Asked again, the removal is finished; there was no project left to count.
            assert_eq!(w.store.remove_project(&dir), Ok(Removed::NotThere));
            assert_eq!(tree(w.store.root()), Vec::<String>::new());
        }

        #[test]
        fn a_damaged_or_newer_project_can_be_removed_and_a_file_somebody_else_put_there_stays() {
            let w = World::new();
            let (damaged, newer, stays) = (key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1));
            for k in [&damaged, &newer, &stays] {
                w.save(k);
            }
            // A project file that names no address, tour or chapter at all.
            std::fs::write(w.dir(&damaged).join(PROJECT_FILE), [0xffu8, 0xfe, 0x00, 0x12]).unwrap();
            std::fs::write(w.dir(&damaged).join("notes.txt"), b"mine").unwrap();
            std::fs::create_dir(w.dir(&damaged).join("my folder")).unwrap();
            std::fs::write(w.dir(&damaged).join(PICTURES).join("mine.txt"), b"mine too").unwrap();
            // A project of a later version, with a file this version has no name for.
            let later = serde_json::json!({"format": 99, "app": "9.0.0", "saved_at": 1_800_000_000_000i64, "server": SITE_A, "tour_id": TOUR_1, "chapter_id": CH_2, "tour_title": "Customer onboarding", "title": "From later", "edit": "something new"}).to_string();
            std::fs::write(w.dir(&newer).join(PROJECT_FILE), later).unwrap();
            std::fs::write(w.dir(&newer).join("frames.later"), b"a later version's own").unwrap();
            let untouched = contents(&w.dir(&stays));

            // The folders come from the store's own list, as the page that asks has them.
            let listed = w.store.for_tour(SITE_A, TOUR_1);
            assert_eq!(listed.iter().map(|p| p.problem).collect::<Vec<_>>(), vec![Some(Problem::Damaged), Some(Problem::Newer)]);
            for info in &listed {
                assert_eq!(w.store.remove_project(&info.dir), Ok(Removed::Now));
            }
            assert_eq!(keys(&w.store.list()), vec![stays.clone()]);
            let tour_dir = w.store.root().join(site_of(SITE_A)).join(TOUR_1);
            assert_eq!(tree(&tour_dir), vec![format!("{CH_1}/"), format!("{CH_1}/my folder/"), format!("{CH_1}/notes.txt"), format!("{CH_1}/pictures/"), format!("{CH_1}/pictures/mine.txt"), format!("{CH_2}/"), format!("{CH_2}/frames.later")], "only files this version writes are removed");
            assert_eq!(contents(&tour_dir).into_iter().map(|(_, bytes)| bytes).collect::<Vec<_>>(), vec![b"mine".to_vec(), b"mine too".to_vec(), b"a later version's own".to_vec()]);
            assert_eq!(contents(&w.dir(&stays)), untouched);
            // What is left is no project: asked again, there is nothing to remove and nothing is.
            assert_eq!(w.store.remove_project(&listed[0].dir), Ok(Removed::NotThere));
            assert_eq!(std::fs::read(w.dir(&damaged).join("notes.txt")).unwrap(), b"mine");
        }

        #[test]
        fn a_folder_outside_the_store_or_with_a_name_the_store_does_not_give_is_refused() {
            let w = World::new();
            w.save(&key_1());
            let (root, dir) = (w.store.root().to_path_buf(), w.dir(&key_1()));
            let (site_dir, tour_dir) = (root.join(site_of(SITE_A)), root.join(site_of(SITE_A)).join(TOUR_1));
            // A whole project under a name, or in a place, that the store never gives one.
            let project = contents(&dir);
            let copy_to = |to: PathBuf| {
                std::fs::create_dir_all(to.join(PICTURES)).unwrap();
                for (name, bytes) in &project {
                    std::fs::write(to.join(name), bytes).unwrap();
                }
                to
            };
            let other_store = w.tmp.path().join("projects-of-another-store");
            #[cfg_attr(not(windows), allow(unused_mut))]
            let mut refused = vec![
                // Outside the store: the editor's own recording, another store with the same layout, the folder above.
                w.tmp.path().join("rec"),
                copy_to(other_store.join(site_of(SITE_A)).join(TOUR_1).join(CH_1)),
                w.tmp.path().to_path_buf(),
                // Inside it, but not a chapter's folder: too high, too deep.
                root.clone(),
                site_dir.clone(),
                tour_dir.clone(),
                dir.join(PICTURES),
                copy_to(dir.join(PICTURES).join(CH_2)),
                // Names the store does not give: a site that is not 16 hex, ids that are not folder ids.
                copy_to(root.join(".staging-1763460000000-0123456789abcdef").join(TOUR_1).join(CH_1)),
                copy_to(root.join(".replaced-0123456789abcdef").join(TOUR_1).join(CH_1)),
                copy_to(root.join("0123456789ABCDEF").join(TOUR_1).join(CH_1)),
                copy_to(root.join("0123456789abcde").join(TOUR_1).join(CH_1)),
                copy_to(site_dir.join("Tour 2").join(CH_1)),
                copy_to(tour_dir.join("ch 2")),
                copy_to(tour_dir.join("ch_2.bak")),
                // Spelled with a way round, or without a full path, or not at all.
                tour_dir.join("..").join(TOUR_1).join(CH_1),
                dir.join("..").join("..").join("..").join("..").join("rec"),
                PathBuf::from(site_of(SITE_A)).join(TOUR_1).join(CH_1),
                PathBuf::from(CH_1),
                PathBuf::new(),
                // Spellings the system would take for the real folder, or for something of its own.
                tour_dir.join(format!("{CH_1}.")),
                tour_dir.join(format!("{CH_1} ")),
                tour_dir.join(format!("{CH_1}::$DATA")),
                tour_dir.join("con"),
                site_dir.join("con").join(CH_1),
            ];
            // The real folder with the prefix for a path taken as it is, when the store's own is not spelled so.
            if !root.to_string_lossy().starts_with(VERBATIM) {
                refused.push(PathBuf::from(format!("{VERBATIM}{}", dir.display())));
            }
            // The real project through another store's eyes: it is not inside that store.
            let elsewhere = own_store(other_store.clone());
            // A chapter and a tour that are links to folders outside the store.
            #[cfg(windows)]
            {
                let outside = copy_to(w.tmp.path().join("outside").join(CH_2));
                link_to(&outside, &tour_dir.join(CH_2));
                let outside_tour = w.tmp.path().join("outside tour");
                copy_to(outside_tour.join(CH_1));
                link_to(&outside_tour, &site_dir.join(TOUR_2));
                refused.push(tour_dir.join(CH_2));
                refused.push(site_dir.join(TOUR_2).join(CH_1));
            }
            let whole = contents(w.tmp.path());
            let before = (changes(&w.store), changes(&elsewhere));
            for path in &refused {
                let err = w.store.remove_project(path).expect_err(&format!("{path:?} was not refused"));
                assert_eq!(err, "this saved recording could not be removed.");
                assert!(reads_as_a_reason(&err, w.tmp.path()));
            }
            assert!(elsewhere.remove_project(&dir).is_err());
            assert_eq!(contents(w.tmp.path()), whole, "nothing anywhere was touched");
            assert_eq!((changes(&w.store), changes(&elsewhere)), before);
            assert_eq!(keys(&w.store.list()), vec![key_1()]);
            // The same folder, named as the store names it, is removed.
            assert_eq!(w.store.remove_project(&dir), Ok(Removed::Now));
        }

        /// The start of a path that the system takes as it is spelled.
        const VERBATIM: &str = r"\\?\";

        /// Everything a saved project holds, written again into the new folder `to`.
        fn copy_project(project: &[(String, Vec<u8>)], to: &Path) {
            std::fs::create_dir_all(to.join(PICTURES)).unwrap();
            for (name, bytes) in project {
                std::fs::write(to.join(name), bytes).unwrap();
            }
            assert!(is_project(to));
        }

        #[cfg(windows)]
        #[test]
        fn a_folder_spelled_otherwise_on_the_disk_than_it_is_asked_for_is_refused() {
            let w = World::new();
            w.save(&key_1());
            let project = contents(&w.dir(&key_1()));
            // By hand, under a name the store never gives; the system finds it under the store's spelling too.
            let site_dir = w.store.root().join(site_of(SITE_A));
            let on_disk = site_dir.join("tour_1").join("CH_1");
            copy_project(&project, &on_disk);
            let asked = site_dir.join("tour_1").join("ch_1");
            assert!(is_project(&asked) && is_folder_id("tour_1") && is_folder_id("ch_1"), "every other check lets it through");
            let (whole, before) = (contents(w.tmp.path()), changes(&w.store));
            assert_eq!(w.store.remove_project(&asked).unwrap_err(), "this saved recording could not be removed.");
            assert!(w.store.remove_project(&on_disk).is_err());
            assert_eq!((contents(w.tmp.path()), changes(&w.store)), (whole, before), "nothing was touched");
        }

        #[test]
        fn a_file_where_a_folder_should_be_is_refused() {
            let w = World::new();
            w.save(&key_1());
            let root = w.store.root().to_path_buf();
            let (site_dir, tour_dir) = (root.join(site_of(SITE_A)), root.join(site_of(SITE_A)).join(TOUR_1));
            // A file with a chapter's name, one with a tour's, and one with a site's.
            for file in [tour_dir.join(CH_2), site_dir.join(TOUR_2), root.join(site_of(SITE_B))] {
                std::fs::write(file, b"mine").unwrap();
            }
            let (whole, before) = (contents(w.tmp.path()), changes(&w.store));
            for asked in [tour_dir.join(CH_2), site_dir.join(TOUR_2).join(CH_1), root.join(site_of(SITE_B)).join(TOUR_1).join(CH_1)] {
                let err = w.store.remove_project(&asked).expect_err(&format!("{asked:?} was not refused"));
                assert_eq!(err, "this saved recording could not be removed.");
            }
            assert_eq!((contents(w.tmp.path()), changes(&w.store)), (whole, before), "nothing was touched");
            assert_eq!(keys(&w.store.list()), vec![key_1()]);
        }

        #[cfg(windows)]
        #[test]
        fn a_link_at_any_level_is_refused_whether_a_folder_link_or_a_junction() {
            fn junction(outside: &Path, link: &Path) -> bool {
                std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(outside).output().is_ok_and(|o| o.status.success())
            }
            fn folder_link(outside: &Path, link: &Path) -> bool {
                std::os::windows::fs::symlink_dir(outside, link).is_ok()
            }
            /// Makes a link at the second path to the folder at the first, and says whether it could.
            type MakeLink = fn(&Path, &Path) -> bool;
            let kinds: [(&str, MakeLink); 2] = [("junction", junction), ("folder link", folder_link)];
            let mut ran = Vec::new();
            for (kind, make) in kinds {
                let w = World::new();
                w.save(&key_1());
                let project = contents(&w.dir(&key_1()));
                let root = w.store.root().to_path_buf();
                let (site_dir, tour_dir) = (root.join(site_of(SITE_A)), root.join(site_of(SITE_A)).join(TOUR_1));
                // Outside the store: a chapter's folder, a tour's and a site's, each with a whole project in it.
                let (chapter, tour, site) = (w.tmp.path().join("outside-chapter"), w.tmp.path().join("outside-tour"), w.tmp.path().join("outside-site"));
                copy_project(&project, &chapter);
                copy_project(&project, &tour.join(CH_1));
                copy_project(&project, &site.join(TOUR_1).join(CH_1));
                let links = [(&chapter, tour_dir.join(CH_2)), (&tour, site_dir.join(TOUR_2)), (&site, root.join(site_of(SITE_B)))];
                if !links.iter().all(|(outside, link)| make(outside, link)) {
                    println!("skipped: a {kind} could not be made on this computer");
                    continue;
                }
                let asked = [tour_dir.join(CH_2), site_dir.join(TOUR_2).join(CH_1), root.join(site_of(SITE_B)).join(TOUR_1).join(CH_1)];
                for path in &asked {
                    assert!(is_project(path), "the {kind} leads to a project");
                }
                let (whole, before) = (contents(w.tmp.path()), changes(&w.store));
                for path in &asked {
                    let err = w.store.remove_project(path).expect_err(&format!("{path:?} behind a {kind} was not refused"));
                    assert_eq!(err, "this saved recording could not be removed.");
                }
                assert_eq!((contents(w.tmp.path()), changes(&w.store)), (whole, before), "nothing behind a {kind} was touched");
                assert_eq!(keys(&w.store.list()), vec![key_1()]);
                println!("ran: {kind}");
                ran.push(kind);
            }
            assert!(ran.contains(&"junction"), "a junction needs no privilege and could not be made");
        }

        #[test]
        fn a_project_marked_for_removal_and_then_saved_again_is_kept() {
            let w = World::new();
            w.save(&key_1());
            let open = loaded(&w.store, &key_1());
            let dir = open.info.dir.clone();
            assert_eq!(w.store.remove_project(&dir), Ok(Removed::WhenItsEditorCloses));
            assert_eq!(w.store.list(), vec![]);
            let (edit, items) = edited(&w);
            let snap = Snapshot { rec: &open.rec, title: "Late", tour_title: "Customer onboarding", steps: 2, edit: &edit, events: &open.restored.events, items: &items };
            // A save that fails changes nothing: the removal still stands.
            let refuse = |_: &Path, _: &Path| -> io::Result<()> { Err(io::Error::other("the rename was refused")) };
            w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &real_frames, rename: &refuse }).unwrap_err();
            assert!(dir.join(REMOVING_FILE).is_file());
            assert_eq!(w.store.list(), vec![]);
            // The editor saves the recording again: that is the user's last word, and it is kept.
            let before = changes(&w.store);
            let saved = w.store.save(&key_1(), &snap).unwrap();
            assert_eq!(changes(&w.store), before + 1);
            assert_eq!((saved.info.title.as_str(), saved.info.key.clone(), saved.info.problem), ("Late", key_1(), None));
            assert!(!dir.join(REMOVING_FILE).exists());
            assert_eq!(w.store.list(), vec![saved.info.clone()]);
            drop(open);
            w.store.finish_removals();
            assert_eq!(changes(&w.store), before + 1, "closing the editor removed nothing");
            assert_eq!(w.store.list(), vec![saved.info]);
            assert_eq!(loaded(&w.store, &key_1()).restored.title, "Late");
        }

        #[test]
        fn a_project_removed_twice_is_not_there_the_second_time() {
            let w = World::new();
            w.save(&key_1());
            w.save(&key(SITE_A, TOUR_1, CH_2));
            let (dir, with_a_note) = (w.dir(&key_1()), w.dir(&key(SITE_A, TOUR_1, CH_2)));
            std::fs::write(with_a_note.join("notes.txt"), b"mine").unwrap();
            assert_eq!(w.store.remove_project(&dir), Ok(Removed::Now));
            assert_eq!(w.store.remove_project(&with_a_note), Ok(Removed::Now));
            let (before, left) = (changes(&w.store), contents(w.store.root()));
            // Its folder is gone; the other's is still there for the note, and is no project.
            assert!(!dir.exists() && with_a_note.is_dir());
            for _ in 0..2 {
                assert_eq!(w.store.remove_project(&dir), Ok(Removed::NotThere));
                assert_eq!(w.store.remove_project(&with_a_note), Ok(Removed::NotThere));
            }
            // A project that never was, and a store whose folder was never made.
            assert_eq!(w.store.remove_project(&w.dir(&key(SITE_B, TOUR_2, CH_2))), Ok(Removed::NotThere));
            let never = own_store(w.tmp.path().join("never made"));
            assert_eq!(never.remove_project(&never.root().join(site_of(SITE_A)).join(TOUR_1).join(CH_1)), Ok(Removed::NotThere));
            assert!(!never.root().exists());
            assert_eq!((changes(&w.store), contents(w.store.root())), (before, left), "nothing was there, so nothing changed");
        }

        #[test]
        fn the_list_no_longer_holds_a_removed_project_and_the_epoch_moved() {
            let w = World::new();
            let removed = w.save(&key(SITE_A, TOUR_1, CH_1)).info;
            let kept = w.save(&key(SITE_A, TOUR_1, CH_2)).info;
            assert_eq!((w.store.list().len(), w.store.totals()), (2, Totals { count: 2, bytes: removed.bytes + kept.bytes }));
            let before = changes(&w.store);
            assert_eq!(w.store.remove_project(&removed.dir), Ok(Removed::Now));
            assert_eq!(changes(&w.store), before + 1, "the pages are told to read the list again");
            assert_eq!(w.store.list(), vec![kept.clone()]);
            assert_eq!(w.store.for_site(SITE_A), vec![kept.clone()]);
            assert_eq!(w.store.for_tour(SITE_A, TOUR_1), vec![kept.clone()]);
            assert_eq!(w.store.totals(), Totals { count: 1, bytes: kept.bytes });
            assert_eq!(load_err(&w.store, &removed.key), LoadError::NotSaved);
        }
    }

    /// A project whose recording is kept as a video: the check, the list, the swap, the
    /// recovery at the next start, opening, saving, candidates and marks.
    mod kept_as_video {
        use super::*;
        use crate::project::state::ConvertMark;
        use mythicshot_recording::{CodecError, form_of};

        /// The bytes of a video file. The store never decodes one itself, so it is not a video.
        const VIDEO: &[u8] = b"not really a video, only as many bytes as the project file names for it";

        fn project_file(dir: &Path) -> ProjectFile {
            state::parse(&std::fs::read_to_string(dir.join(PROJECT_FILE)).unwrap()).unwrap()
        }

        fn write_project(dir: &Path, file: &ProjectFile) {
            std::fs::write(dir.join(PROJECT_FILE), state::to_text(file).unwrap()).unwrap();
        }

        /// By hand, what a finished swap leaves of the frames-form project in `dir`.
        fn as_video(dir: &Path) {
            let rec = Recording::open_saved(dir).unwrap();
            let mut file = project_file(dir);
            std::fs::write(dir.join(VIDEO_FILE), VIDEO).unwrap();
            file.format = state::FORMAT;
            file.store = StoreInfo { form: SavedForm::Video, video_bytes: VIDEO.len() as u64, lossless_bytes: file.frames_bytes, convert: None };
            write_project(dir, &file);
            rec.write_video_index(dir, VIDEO.len() as u64).unwrap();
            std::fs::remove_file(dir.join(FRAMES_FILE)).unwrap();
        }

        fn size_of(dir: &Path) -> u64 {
            contents(dir).iter().map(|(_, bytes)| bytes.len() as u64).sum()
        }

        #[test]
        fn a_video_form_project_is_listed_with_its_form_and_size() {
            let w = World::new();
            let saved = w.save(&key_1());
            assert_eq!((saved.info.form, saved.info.convert), (Form::Frames, ConvertState::NotTried));
            assert_eq!(w.store.list(), vec![saved.info.clone()]);
            let dir = w.dir(&key_1());
            as_video(&dir);
            let list = w.store.list();
            assert_eq!(list.len(), 1);
            let p = &list[0];
            assert_eq!((p.form, p.convert, p.problem), (Form::Video, ConvertState::Done, None));
            assert_eq!((p.key.clone(), p.title.as_str(), p.tour_title.as_str(), p.saved_at, p.dir.clone()), (key_1(), "Chapter one", "Customer onboarding", saved.info.saved_at, dir.clone()));
            // The size is what the folder holds now: the video and no frames file.
            assert_eq!(p.bytes, size_of(&dir));
            assert!(p.bytes < saved.info.bytes);
            assert_eq!((w.store.for_site(SITE_A), w.store.for_tour(SITE_A, TOUR_1)), (list.clone(), list.clone()));
            assert_eq!(w.store.totals(), Totals { count: 1, bytes: p.bytes });
            // What a swap cut off before its last two steps leaves is the same project: the
            // index of the frames form holds the same times, and the frames file is not looked at.
            let (frames, index) = frames_and_index(FRAMES);
            std::fs::write(dir.join(FRAMES_FILE), frames).unwrap();
            std::fs::write(dir.join(INDEX_FILE), index).unwrap();
            let cut_off = w.store.list();
            assert_eq!((cut_off[0].form, cut_off[0].problem, cut_off[0].bytes), (Form::Video, None, size_of(&dir)));
        }

        #[test]
        fn a_video_cut_short_or_deleted_by_hand_is_a_problem_not_a_crash() {
            let w = World::new();
            let all = [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1), key(SITE_B, TOUR_1, CH_1), key(SITE_B, TOUR_1, CH_2), key(SITE_B, TOUR_2, CH_1), key(SITE_B, TOUR_2, CH_2)];
            for k in &all {
                w.save(k);
                as_video(&w.dir(k));
            }
            let [deleted, cut, longer, no_index, no_size, huge, a_folder] = all.clone().map(|k| w.dir(&k));
            std::fs::remove_file(deleted.join(VIDEO_FILE)).unwrap();
            std::fs::write(cut.join(VIDEO_FILE), &VIDEO[..VIDEO.len() - 7]).unwrap();
            std::fs::write(longer.join(VIDEO_FILE), [VIDEO, b"and more"].concat()).unwrap();
            std::fs::remove_file(no_index.join(INDEX_FILE)).unwrap();
            // A project file that names no size for its video, beside a video of no size.
            let mut file = project_file(&no_size);
            file.store.video_bytes = 0;
            write_project(&no_size, &file);
            std::fs::write(no_size.join(VIDEO_FILE), b"").unwrap();
            // Sizes written by hand that no file has.
            let mut file = project_file(&huge);
            (file.frames_bytes, file.store.video_bytes, file.store.lossless_bytes) = (u64::MAX, u64::MAX, u64::MAX);
            write_project(&huge, &file);
            // Where the video should be, there is a folder.
            std::fs::remove_file(a_folder.join(VIDEO_FILE)).unwrap();
            std::fs::create_dir(a_folder.join(VIDEO_FILE)).unwrap();
            let before = contents(w.store.root());

            let list = w.store.list();
            assert_eq!(list.len(), all.len());
            for p in &list {
                assert_eq!((p.problem, p.form, p.title.as_str()), (Some(Problem::FramesMissing), Form::Video, "Chapter one"), "{:?}", p.dir);
            }
            assert_eq!(w.store.totals(), Totals { count: all.len(), bytes: list.iter().map(|p| p.bytes).sum() });
            for k in &all {
                assert!(matches!(load_err(&w.store, k), LoadError::Damaged(_)), "{k:?}");
                // And it is not left registered as open.
                assert!(matches!(load_err(&w.store, k), LoadError::Damaged(_)));
            }
            assert_eq!(contents(w.store.root()), before, "nothing is repaired or removed");
        }

        #[test]
        fn a_project_from_format_3_is_listed_as_newer_and_never_touched() {
            let w = World::new();
            w.save(&key_1());
            let dir = w.dir(&key_1());
            // A later version's project: a form this version has no word for, beside files it knows.
            let newer = serde_json::json!({"format": 3, "app": "9.0.0", "saved_at": 1_800_000_000_000i64, "server": SITE_A, "tour_id": TOUR_1, "chapter_id": CH_1, "tour_title": "Customer onboarding", "title": "From later", "store": {"form": "video", "video_bytes": VIDEO.len(), "lossless_bytes": 245}, "edit": "something new"}).to_string();
            std::fs::write(dir.join(PROJECT_FILE), &newer).unwrap();
            std::fs::write(dir.join(VIDEO_FILE), VIDEO).unwrap();
            let whole = contents(&dir);

            let list = w.store.list();
            assert_eq!(list.len(), 1);
            assert_eq!((list[0].problem, list[0].key.clone(), list[0].title.as_str(), list[0].saved_at), (Some(Problem::Newer), key_1(), "From later", 1_800_000_000_000));
            assert_eq!(list[0].convert, ConvertState::NotTried);
            assert_eq!(load_err(&w.store, &key_1()), LoadError::Newer);
            let err = w.store.save(&key_1(), &w.snap()).unwrap_err();
            assert!(err.contains("newer") && err.ends_with('.'), "{err}");
            // The next start and the conversion queue leave it alone as well.
            w.store.finish_conversions();
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), None);
            assert_eq!(w.store.form_of(&key_1()), None, "what a later version wrote about the form is not read");
            assert_eq!(contents(&dir), whole);
            // The same file with the number this version reads is a project again: only the number decides.
            std::fs::write(dir.join(PROJECT_FILE), newer.replace("\"format\":3", "\"format\":2")).unwrap();
            assert_ne!(w.store.list()[0].problem, Some(Problem::Newer));
        }

        /// A frames file of exactly `total` bytes in ten frames, and the index that describes it.
        fn frames_of_size(total: usize) -> (Vec<u8>, String) {
            let (mut bytes, mut rows) = (Vec::new(), Vec::new());
            for i in 0..10 {
                let len = if i < 9 { total / 10 } else { total - 9 * (total / 10) };
                rows.push(serde_json::json!([bytes.len(), len, i == 0, i * 100, null, null]));
                bytes.extend(vec![i as u8 + 1; len]);
            }
            (bytes, serde_json::json!({"format": 1, "width": 100, "height": 80, "end_ms": 1000, "frames": rows}).to_string())
        }

        #[test]
        fn every_format_1_project_of_0_2_7_still_lists_loads_and_saves() {
            // The file 0.2.7 writes, as the format's golden file pins it, in a folder built by hand.
            const GOLDEN: &str = include_str!("golden-v1.json");
            let w = World::new();
            let dir = w.dir(&key_1());
            std::fs::create_dir_all(&dir).unwrap();
            let (frames, index) = frames_of_size(123_456);
            std::fs::write(dir.join(PROJECT_FILE), GOLDEN).unwrap();
            std::fs::write(dir.join(FRAMES_FILE), &frames).unwrap();
            std::fs::write(dir.join(INDEX_FILE), &index).unwrap();
            let whole = contents(&dir);

            let list = w.store.list();
            assert_eq!(list.len(), 1);
            assert_eq!((list[0].key.clone(), list[0].title.as_str(), list[0].saved_at, list[0].problem), (key_1(), "Chapter one", 1_763_460_000_000, None));
            assert_eq!((list[0].form, list[0].convert, list[0].bytes), (Form::Frames, ConvertState::NotTried, size_of(&dir)));
            let l = loaded(&w.store, &key_1());
            assert_eq!((l.restored.title.as_str(), l.restored.edit.trim, l.rec.frames.len()), ("Chapter one", (1, 8), 10));
            assert_eq!(contents(&dir), whole, "listing and opening rewrite nothing");

            // An Update from the editor that opened it: still format 1, with no word of the store
            // section, and the text is the one the format writes for the frames form.
            let snap = Snapshot { rec: &l.rec, title: "Chapter one, again", tour_title: "Customer onboarding", steps: 2, edit: &l.restored.edit, events: &l.restored.events, items: &l.restored.items };
            let saved = w.store.save(&key_1(), &snap).unwrap();
            assert_eq!((saved.info.form, saved.info.convert, saved.info.problem), (Form::Frames, ConvertState::NotTried, None));
            let text = std::fs::read_to_string(dir.join(PROJECT_FILE)).unwrap();
            let file = project_file(&dir);
            assert_eq!((file.format, file.store.clone(), file.frames_bytes), (1, StoreInfo::default(), 123_456));
            assert!(text.starts_with("{\n  \"format\": 1,\n") && !text.contains("\"store\"") && !text.contains("convert"), "{text}");
            assert_eq!(text, state::to_text(&state::to_file(&key_1(), &snap, file.saved_at, 123_456, StoreInfo::default()).0).unwrap());
            assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), frames);
            assert_eq!(std::fs::read_to_string(dir.join(INDEX_FILE)).unwrap(), index);
            drop(l);
            assert_eq!(loaded(&w.store, &key_1()).restored.title, "Chapter one, again");

            // A first save by this version is the frames form too: the files 0.2.7 writes and no other.
            let fresh = w.save(&key(SITE_A, TOUR_2, CH_1));
            let fresh_dir = w.dir(&key(SITE_A, TOUR_2, CH_1));
            let text = std::fs::read_to_string(fresh_dir.join(PROJECT_FILE)).unwrap();
            assert!(text.starts_with("{\n  \"format\": 1,\n") && !text.contains("\"store\""), "{text}");
            assert_eq!(text, state::to_text(&state::to_file(&key(SITE_A, TOUR_2, CH_1), &w.snap(), fresh.info.saved_at, frames_and_index(FRAMES).0.len() as u64, StoreInfo::default()).0).unwrap());
            assert!(tree(&fresh_dir).iter().all(|n| n != VIDEO_FILE));
            let json = |path: PathBuf| serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(json(fresh_dir.join(INDEX_FILE)), json(w.tmp.path().join("rec").join(INDEX_FILE)));
        }

        fn marked(dir: &Path, state: &str, codec: &str, app: &str) {
            let mut file = project_file(dir);
            file.store.convert = Some(ConvertMark { state: state.into(), codec: codec.into(), app: app.into(), at: 1_763_460_000_000 });
            write_project(dir, &file);
        }

        #[test]
        fn how_a_conversion_ended_is_told_only_for_this_codec_and_this_version() {
            let w = World::new();
            w.save(&key_1());
            let dir = w.dir(&key_1());
            let state_now = |w: &World| w.store.list()[0].convert;
            let app = env!("CARGO_PKG_VERSION");
            for (state, codec, version, want) in [
                (MARK_FAILED, TEST_CODEC, app, ConvertState::Failed),
                (MARK_NOT_SMALLER, TEST_CODEC, app, ConvertState::NotSmaller),
                // Another codec, or another version, tried: this one has not.
                (MARK_FAILED, "another-codec", app, ConvertState::NotTried),
                (MARK_NOT_SMALLER, TEST_CODEC, "0.0.1", ConvertState::NotTried),
                // A word this version does not write is no mark.
                ("done", TEST_CODEC, app, ConvertState::NotTried),
                ("", TEST_CODEC, app, ConvertState::NotTried),
            ] {
                marked(&dir, state, codec, version);
                assert_eq!(state_now(&w), want, "{state} {codec} {version}");
                assert_eq!((w.store.list()[0].problem, w.store.list()[0].form), (None, Form::Frames));
            }
            // A store that knows no codec (ffmpeg was not found) says nothing was tried.
            marked(&dir, MARK_FAILED, TEST_CODEC, app);
            let plain = Store { codec: Some(None), ..w.store.clone() };
            assert_eq!(plain.list()[0].convert, ConvertState::NotTried);
            // A mark is still a format 1 file that loads.
            assert_eq!(project_file(&dir).format, 1);
            loaded(&w.store, &key_1());
        }

        #[test]
        fn the_codec_in_use_is_told_to_every_store_once() {
            let w = World::new();
            w.save(&key_1());
            marked(&w.dir(&key_1()), MARK_FAILED, "a-codec-of-this-test-only", env!("CARGO_PKG_VERSION"));
            // A store as the app makes one: it knows the codec the app found, and no other.
            let app_store = Store::at(w.store.root().to_path_buf());
            assert_eq!(app_store.list()[0].convert, ConvertState::NotTried);
            let before = epoch();
            set_current_codec(Some("a-codec-of-this-test-only"));
            assert_eq!(app_store.list()[0].convert, ConvertState::Failed);
            // What the pages show of a recording depends on the codec: they are told to read again.
            let named = epoch();
            assert!(named > before, "{before} {named}");
            set_current_codec(None);
            assert_eq!(app_store.list()[0].convert, ConvertState::NotTried);
            assert!(epoch() > named);
        }

        #[test]
        fn a_mark_that_does_not_fit_its_shape_costs_only_the_mark() {
            let w = World::new();
            w.save(&key_1());
            let dir = w.dir(&key_1());
            let text = std::fs::read_to_string(dir.join(PROJECT_FILE)).unwrap();
            for mark in ["\"yes\"", "17", "[17]", "{\"state\": 5}", "{\"state\": \"failed\", \"at\": \"yesterday\"}"] {
                let with_mark = text.replacen("\"edit\": {", &format!("\"store\": {{\"convert\": {mark}}},\n  \"edit\": {{"), 1);
                assert_ne!(with_mark, text);
                assert!(state::parse(&with_mark).is_err(), "{mark} is not a mark");
                std::fs::write(dir.join(PROJECT_FILE), &with_mark).unwrap();
                let list = w.store.list();
                assert_eq!((list[0].problem, list[0].convert, list[0].form, list[0].title.as_str()), (None, ConvertState::NotTried, Form::Frames, "Chapter one"), "{mark}");
                assert_eq!(loaded(&w.store, &key_1()).restored.title, "Chapter one");
                assert_eq!(std::fs::read_to_string(dir.join(PROJECT_FILE)).unwrap(), with_mark, "reading it rewrites nothing");
            }
            // Anything else that does not fit is still a damaged file.
            std::fs::write(dir.join(PROJECT_FILE), text.replacen("\"edit\": {", "\"store\": {\"form\": \"hologram\", \"convert\": 17},\n  \"edit\": {", 1)).unwrap();
            assert_eq!(w.store.list()[0].problem, Some(Problem::Damaged));
        }

        // ---- The swap, and the next start ----

        /// One conversion at a time in the whole process: a test that starts one waits its turn.
        fn one_conversion() -> MutexGuard<'static, ()> {
            static ONE: Mutex<()> = Mutex::new(());
            ONE.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// What the queue holds of the project in `dir` when it starts on it.
        fn candidate(key: &ProjectKey, dir: &Path) -> Candidate {
            let file = project_file(dir);
            Candidate { key: key.clone(), dir: dir.to_path_buf(), saved_at: file.saved_at, frames_bytes: file.frames_bytes }
        }

        /// Start a conversion and put a finished video into its folder.
        fn converted(w: &World, key: &ProjectKey) -> (Candidate, Converting) {
            let c = candidate(key, &w.dir(key));
            let conv = w.store.begin_conversion(&c).expect("the conversion did not start");
            std::fs::write(conv.video_path(), VIDEO).unwrap();
            (c, conv)
        }

        /// Convert the project of `key` the whole way, as the queue does.
        fn swap(w: &World, key: &ProjectKey) {
            let (c, conv) = converted(w, key);
            assert_eq!(w.store.swap_to_video(&c, &conv, VIDEO.len() as u64), Ok(true));
        }

        /// Hands out `frames` pictures, each filled with its own number; or does what it is told.
        #[derive(Default)]
        struct Fake<'a> {
            fails: Option<fn() -> CodecError>,
            short_by: usize,
            during: Option<Box<dyn Fn() + Sync + 'a>>,
        }

        impl VideoCodec for Fake<'_> {
            fn id(&self) -> &str {
                TEST_CODEC
            }

            fn encode(&self, _: (u32, u32), _: usize, _: &mut dyn FnMut(&mut Vec<u8>) -> Result<bool, String>, _: &Path, _: u64, _: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
                unreachable!("the store never encodes")
            }

            fn decode(&self, video: &Path, size: (u32, u32), frames: usize, each: &mut dyn FnMut(usize, &[u8]) -> Result<(), String>, keep_going: &dyn Fn(usize) -> bool) -> Result<(), CodecError> {
                assert_eq!(std::fs::read(video).unwrap(), VIDEO, "the project's own video is what is decoded");
                if let Some(fails) = self.fails {
                    return Err(fails());
                }
                if let Some(during) = &self.during {
                    during();
                }
                for i in 0..frames - self.short_by {
                    if !keep_going(i) {
                        return Err(CodecError::Cancelled);
                    }
                    each(i, &vec![i as u8; (size.0 * size.1 * 4) as usize]).map_err(CodecError::Caller)?;
                }
                if self.short_by > 0 { Err(CodecError::WrongFrames { got: frames - self.short_by }) } else { Ok(()) }
            }
        }

        fn go(_: usize, _: usize) -> bool {
            true
        }

        fn open_with(store: &Store, key: &ProjectKey, codec: &dyn VideoCodec, temp: &Path) -> Result<Loaded, LoadError> {
            store.load_with(key, &Expand { codec: Some(codec), temp, keep_going: &go })
        }

        fn why(result: Result<Loaded, LoadError>) -> LoadError {
            match result {
                Ok(_) => panic!("the project opened"),
                Err(e) => e,
            }
        }

        /// The names in a project folder that is entirely its frames form, and entirely its video form.
        fn frames_form(dir: &Path) -> bool {
            let names = tree(dir);
            names.iter().any(|n| n == FRAMES_FILE) && names.iter().all(|n| n != VIDEO_FILE && !n.ends_with(".tmp")) && project_file(dir).format == 1 && form_of(dir) == Ok(Form::Frames)
        }

        fn video_form(dir: &Path) -> bool {
            let names = tree(dir);
            names.iter().any(|n| n == VIDEO_FILE) && names.iter().all(|n| n != FRAMES_FILE && !n.ends_with(".tmp")) && project_file(dir).format == 2 && form_of(dir) == Ok(Form::Video)
        }

        /// A swap of the project of `key_1` in which the disk is copied before and after every
        /// rename, and at the end: each copy is what a crash at that moment leaves. The copies,
        /// by name, each a projects folder of its own.
        fn crash_points(w: &World) -> Vec<(String, Store)> {
            let _one = one_conversion();
            let (c, conv) = converted(w, &key_1());
            let (count, shots) = (std::cell::Cell::new(0), std::cell::RefCell::new(Vec::new()));
            let shot = |name: String| {
                let root = w.tmp.path().join(format!("crash {name}")).join("projects");
                copy_tree(w.store.root(), &root);
                shots.borrow_mut().push((name, own_store(root)));
            };
            let rename = |from: &Path, to: &Path| {
                count.set(count.get() + 1);
                shot(format!("before rename {}", count.get()));
                std::fs::rename(from, to)?;
                shot(format!("after rename {}", count.get()));
                Ok(())
            };
            assert_eq!(w.store.swap_with(&c, &conv, VIDEO.len() as u64, &Steps { keep_frames: &real_frames, rename: &rename }), Ok(true));
            assert_eq!(count.get(), 3, "the video in, the project file, the index");
            drop(conv);
            shot("done".to_string());
            shots.into_inner()
        }

        /// The project of `key_1` in a crash copy lists without a problem and opens, with the
        /// saved state; returns the form it is listed in.
        fn lists_and_loads(name: &str, store: &Store, tmp: &Path, round: &str) -> Form {
            let list = store.list();
            assert_eq!(list.len(), 1, "{name}");
            assert_eq!((list[0].problem, list[0].key.clone(), list[0].title.as_str()), (None, key_1(), "Chapter one"), "{name} {round}");
            let temp = tmp.join(format!("edit {name} {round}"));
            let l = open_with(store, &key_1(), &Fake::default(), &temp).unwrap_or_else(|e| panic!("{name} {round}: {e:?}"));
            assert_eq!((l.rec.frames.len(), l.rec.end_ms, l.restored.edit.trim, l.restored.title.as_str()), (FRAMES, 1000, (1, 8), "Chapter one"), "{name} {round}");
            assert_eq!(l.rec.frames.iter().map(|m| m.t_ms).collect::<Vec<_>>(), (0..FRAMES as u64).map(|i| i * 100).collect::<Vec<_>>());
            list[0].form
        }

        #[test]
        fn a_swap_cut_off_before_the_project_file_is_renamed_leaves_a_frames_project_that_loads() {
            let w = World::new();
            w.save(&key_1());
            let whole = contents(&w.dir(&key_1()));
            let before: Vec<_> = crash_points(&w).into_iter().filter(|(name, _)| ["before rename 1", "after rename 1", "before rename 2"].contains(&name.as_str())).collect();
            assert_eq!(before.len(), 3);
            for (name, store) in &before {
                let dir = store.root().join(site_of(SITE_A)).join(TOUR_1).join(CH_1);
                // Entirely its frames form: every file it had, byte for byte, and at most the video and a temporary file more.
                assert_eq!(contents(&dir).into_iter().filter(|(n, _)| n != VIDEO_FILE && !n.ends_with(".tmp")).collect::<Vec<_>>(), whole, "{name}");
                assert_eq!(lists_and_loads(name, store, w.tmp.path(), "as left"), Form::Frames, "{name}");
                assert_eq!(in_root(store, CONVERTING).len(), 1, "{name}: the conversion's own folder is all that is left over outside the project");
                // The next start: the extra file goes, the conversion's folder goes when it is old, and nothing else.
                store.recover_replaced();
                store.finish_removals();
                store.finish_conversions();
                store.sweep_converting_at(DAY, SystemTime::now() + 2 * DAY);
                assert_eq!(contents(&dir), whole, "{name}");
                assert!(frames_form(&dir), "{name}");
                assert_eq!(tree(store.root()).into_iter().filter(|n| n.starts_with('.')).collect::<Vec<_>>(), Vec::<String>::new(), "{name}");
                assert_eq!(lists_and_loads(name, store, w.tmp.path(), "after the next start"), Form::Frames, "{name}");
                // It is a candidate again: the conversion is simply made another time.
                assert_eq!(store.next_candidate(TEST_CODEC, 0).map(|c| c.key), Some(key_1()), "{name}");
            }
        }

        #[test]
        fn a_swap_cut_off_after_it_leaves_a_video_project_that_loads_and_the_next_start_finishes_it() {
            let w = World::new();
            w.save(&key_1());
            let points = crash_points(&w);
            assert_eq!(points.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["before rename 1", "after rename 1", "before rename 2", "after rename 2", "before rename 3", "after rename 3", "done"]);
            let finished = contents(&w.dir(&key_1()));
            for (name, store) in &points[3..] {
                let dir = store.root().join(site_of(SITE_A)).join(TOUR_1).join(CH_1);
                // Entirely its video form: the project file names the video, and the video is whole.
                let file = project_file(&dir);
                assert_eq!((file.format, file.store.form, file.store.video_bytes, file.store.lossless_bytes), (2, SavedForm::Video, VIDEO.len() as u64, 245), "{name}");
                assert_eq!(std::fs::read(dir.join(VIDEO_FILE)).unwrap(), VIDEO, "{name}");
                assert_eq!(lists_and_loads(name, store, w.tmp.path(), "as left"), Form::Video, "{name}");
                assert_eq!(dir.join(FRAMES_FILE).exists(), name != "done", "{name}: the frames go last");
                store.recover_replaced();
                store.finish_removals();
                store.finish_conversions();
                store.sweep_converting_at(DAY, SystemTime::now() + 2 * DAY);
                assert_eq!(contents(&dir), finished, "{name}: the next start leaves what a whole swap leaves");
                assert!(video_form(&dir), "{name}");
                assert_eq!(tree(store.root()).into_iter().filter(|n| n.starts_with('.')).collect::<Vec<_>>(), Vec::<String>::new(), "{name}");
                assert_eq!(lists_and_loads(name, store, w.tmp.path(), "after the next start"), Form::Video, "{name}");
                assert_eq!(store.next_candidate(TEST_CODEC, 0), None, "{name}");
                // A second start changes nothing more.
                let epoch = changes(store);
                store.finish_conversions();
                assert_eq!((contents(&dir), changes(store)), (finished.clone(), epoch), "{name}");
            }
        }

        #[test]
        fn a_step_of_the_swap_that_fails_leaves_the_project_whole_in_one_form() {
            // The same steps, each refused in turn instead of cut off.
            for failing in 1..=3 {
                let _one = one_conversion();
                let w = World::new();
                w.save(&key_1());
                let dir = w.dir(&key_1());
                let whole = contents(&dir);
                let (c, conv) = converted(&w, &key_1());
                let count = std::cell::Cell::new(0);
                let rename = |from: &Path, to: &Path| {
                    count.set(count.get() + 1);
                    if count.get() == failing { Err(no_room()) } else { std::fs::rename(from, to) }
                };
                let result = w.store.swap_with(&c, &conv, VIDEO.len() as u64, &Steps { keep_frames: &real_frames, rename: &rename });
                // The queue gives the conversion up; its folder goes with it.
                drop(conv);
                if failing < 3 {
                    // Before the project file names the video: an error, and the project is as it was.
                    assert_eq!(result, Err(DISK_FULL.to_string()), "step {failing}");
                    assert_eq!(contents(&dir), whole, "step {failing}");
                    assert_eq!(lists_and_loads("refused", &w.store, w.tmp.path(), &failing.to_string()), Form::Frames);
                } else {
                    // After it: the project is its video form, and the frames stay until the next start.
                    assert_eq!(result, Ok(true));
                    assert!(dir.join(FRAMES_FILE).is_file() && form_of(&dir) == Ok(Form::Frames));
                    assert_eq!(lists_and_loads("refused", &w.store, w.tmp.path(), "3"), Form::Video);
                    w.store.finish_conversions();
                    assert!(video_form(&dir));
                    assert_eq!(lists_and_loads("refused", &w.store, w.tmp.path(), "3 again"), Form::Video);
                }
                assert_eq!(in_root(&w.store, CONVERTING), Vec::<PathBuf>::new(), "step {failing}");
            }
        }

        #[cfg(windows)]
        #[test]
        fn a_frames_file_that_cannot_be_removed_yet_is_removed_at_the_next_start() {
            use std::os::windows::fs::OpenOptionsExt;
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            let dir = w.dir(&key_1());
            // Another program has the frames file open and does not let it be deleted.
            let held = std::fs::OpenOptions::new().read(true).share_mode(1 | 2).open(dir.join(FRAMES_FILE)).unwrap();
            swap(&w, &key_1());
            assert!(dir.join(FRAMES_FILE).is_file() && form_of(&dir) == Ok(Form::Video));
            assert_eq!(w.store.list()[0].form, Form::Video);
            drop(held);
            w.store.finish_conversions();
            assert!(video_form(&dir));
        }

        #[test]
        fn after_a_swap_the_frames_file_is_gone_and_the_folder_holds_only_what_the_store_writes() {
            let _one = one_conversion();
            let w = World::new();
            let saved = w.save(&key_1());
            let dir = w.dir(&key_1());
            let before = contents(&dir);
            std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
            assert_eq!(converting_now(), None);
            let epoch = changes(&w.store);
            let (c, conv) = converted(&w, &key_1());
            assert_eq!((converting_now(), changes(&w.store)), (Some(dir.clone()), epoch + 1));
            assert!(conv.work_dir().parent() == Some(w.store.root()) && converting_started(conv.work_dir().file_name().unwrap().to_str().unwrap()).is_some(), "{:?}", conv.work_dir());
            assert_eq!(w.store.swap_to_video(&c, &conv, VIDEO.len() as u64), Ok(true));
            assert_eq!(changes(&w.store), epoch + 2);
            drop(conv);
            assert_eq!((converting_now(), changes(&w.store)), (None, epoch + 3));

            let picture = picture_names(&dir);
            let mut want = vec![VIDEO_FILE.to_string(), logo_name(), "notes.txt".to_string(), "pictures/".to_string(), format!("pictures/{}", picture[0]), PROJECT_FILE.to_string(), INDEX_FILE.to_string()];
            want.sort();
            assert_eq!(tree(&dir), want);
            assert_eq!(tree(w.store.root()).into_iter().filter(|n| n.starts_with('.')).collect::<Vec<_>>(), Vec::<String>::new(), "the conversion's folder is gone");
            assert_eq!(std::fs::read(dir.join(VIDEO_FILE)).unwrap(), VIDEO);
            // The pictures and the logo are the bytes they were; the project file differs only in its form.
            let after = contents(&dir);
            for (name, bytes) in before.iter().filter(|(name, _)| ![PROJECT_FILE, INDEX_FILE, FRAMES_FILE].contains(&name.as_str())) {
                assert_eq!(after.iter().find(|(n, _)| n == name).map(|(_, b)| b), Some(bytes), "{name}");
            }
            let (mut file, was) = (project_file(&dir), state::parse(std::str::from_utf8(&before.iter().find(|(n, _)| n == PROJECT_FILE).unwrap().1).unwrap()).unwrap());
            assert_eq!((file.format, file.store.clone()), (2, StoreInfo { form: SavedForm::Video, video_bytes: VIDEO.len() as u64, lossless_bytes: 245, convert: None }));
            (file.format, file.store) = (1, StoreInfo::default());
            assert_eq!(file, was, "nothing else in the project file changed, the time of saving included");
            // The index is the video form's, with every time and pointer the frames index had.
            let index = mythicshot_recording::open_video_index(&dir).unwrap();
            assert_eq!((index.video_bytes, index.times.len(), index.end_ms, index.width, index.height), (Some(VIDEO.len() as u64), FRAMES, 1000, 100, 80));
            // By hand, the same thing comes out.
            let twin = key(SITE_A, TOUR_2, CH_1);
            w.save(&twin);
            as_video(&w.dir(&twin));
            assert_eq!(std::fs::read(w.dir(&twin).join(INDEX_FILE)).unwrap(), std::fs::read(dir.join(INDEX_FILE)).unwrap());

            let list = w.store.for_tour(SITE_A, TOUR_1);
            assert_eq!((list[0].form, list[0].convert, list[0].problem, list[0].saved_at, list[0].bytes), (Form::Video, ConvertState::Done, None, saved.info.saved_at, size_of(&dir)));
            assert_eq!(w.store.form_of(&key_1()), Some(Form::Video));
            // The next start finds nothing to do.
            let (whole, epoch) = (contents(&dir), changes(&w.store));
            w.store.finish_conversions();
            w.store.sweep_converting(Duration::ZERO);
            assert_eq!((contents(&dir), changes(&w.store)), (whole, epoch));
        }

        #[test]
        fn a_project_opened_saved_over_or_marked_for_removal_meanwhile_is_not_swapped() {
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            let dir = w.dir(&key_1());
            let len = VIDEO.len() as u64;
            let refused = |what: &str, c: &Candidate, conv: &Converting| {
                let whole = contents(&dir);
                assert_eq!(w.store.swap_to_video(c, conv, len), Ok(false), "{what}");
                assert_eq!(w.store.mark_conversion(c, MARK_FAILED, TEST_CODEC), Ok(false), "{what}");
                assert_eq!(contents(&dir), whole, "{what}: nothing was touched");
                assert_eq!(std::fs::read(conv.video_path()).unwrap(), VIDEO, "{what}: not even the video");
            };

            // Opened in an editor: the conversion is told to stop, and a swap is refused all the same.
            let (c, conv) = converted(&w, &key_1());
            assert!(!conv.cancelled());
            let open = loaded(&w.store, &key_1());
            assert!(conv.cancelled(), "Edit cancels the conversion of that project");
            refused("open", &c, &conv);
            drop(conv);
            assert_eq!(w.store.begin_conversion(&c).map(|_| ()), None, "an open project is not started on");
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), None);
            drop(open);

            // The video is not the size the queue verified.
            let (c, conv) = converted(&w, &key_1());
            assert_eq!(w.store.swap_to_video(&c, &conv, len + 1), Ok(false));
            assert_eq!(w.store.swap_to_video(&c, &conv, 0), Ok(false));
            assert_eq!(w.store.swap_to_video(&c, &conv, len - 1), Ok(false));
            // Another project's conversion is not this one's.
            let other = Candidate { key: key(SITE_A, TOUR_1, CH_2), ..c.clone() };
            assert_eq!(w.store.swap_to_video(&other, &conv, len), Ok(false));
            let elsewhere = Candidate { dir: w.tmp.path().join("rec"), ..c.clone() };
            assert_eq!(w.store.swap_to_video(&elsewhere, &conv, len), Ok(false));
            // The frames file is no longer the size the conversion read.
            let frames = std::fs::read(dir.join(FRAMES_FILE)).unwrap();
            std::fs::write(dir.join(FRAMES_FILE), &frames[..frames.len() - 1]).unwrap();
            refused("frames cut", &c, &conv);
            std::fs::write(dir.join(FRAMES_FILE), &frames).unwrap();
            // Marked for removal.
            mark_removing(&dir).unwrap();
            refused("marked for removal", &c, &conv);
            std::fs::remove_file(dir.join(REMOVING_FILE)).unwrap();
            // Saved again meanwhile, by hand here: another time of saving.
            saved_later(&dir, 1);
            refused("saved again", &c, &conv);
            drop(conv);
            assert_eq!(w.store.begin_conversion(&c).map(|_| ()), None, "what was read is no longer what is there");
            saved_later(&dir, -1);

            // Told to stop: no swap, whatever else holds.
            let (c, conv) = converted(&w, &key_1());
            cancel_conversion_of(&dir, Duration::ZERO);
            assert!(conv.cancelled());
            let whole = contents(&dir);
            assert_eq!(w.store.swap_to_video(&c, &conv, len), Ok(false));
            assert_eq!(contents(&dir), whole);
            drop(conv);

            // Saved over from another recording while it was converted: the save stops the
            // conversion (and waits for it a moment), and the swap finds another project.
            let (c, conv) = converted(&w, &key_1());
            let other = recorded(w.tmp.path(), "other", 4);
            let edit = Edit::new(&other, Default::default());
            w.store.save(&key_1(), &Snapshot { rec: &other, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] }).unwrap();
            assert!(conv.cancelled());
            refused("saved over", &c, &conv);
            drop(conv);
            // Removed while it was converted.
            let (c, conv) = converted(&w, &key_1());
            assert_eq!(w.store.remove_project(&dir), Ok(Removed::Now));
            assert!(conv.cancelled());
            assert_eq!(w.store.swap_to_video(&c, &conv, len), Ok(false));
            drop(conv);
            assert_eq!(tree(w.store.root()), Vec::<String>::new());
            assert_eq!(converting_now(), None);
        }

        #[test]
        fn only_one_conversion_runs_at_a_time_and_its_folder_goes_with_it() {
            let _one = one_conversion();
            let w = World::new();
            let (first, second) = (key_1(), key(SITE_A, TOUR_1, CH_2));
            w.save(&first);
            w.save(&second);
            let (c, conv) = converted(&w, &first);
            let work = conv.work_dir().to_path_buf();
            assert!(w.store.begin_conversion(&candidate(&second, &w.dir(&second))).is_none());
            assert!(w.store.begin_conversion(&c).is_none());
            // Telling another project's conversion to stop does nothing to this one.
            cancel_conversion_of(&w.dir(&second), Duration::ZERO);
            assert!(!conv.cancelled());
            // A sweep never takes the folder of the conversion that runs, however old it says it is.
            w.store.sweep_converting_at(Duration::ZERO, SystemTime::now() + FAR);
            assert_eq!(std::fs::read(conv.video_path()).unwrap(), VIDEO);
            // Given up: the folder and what was in it are gone, the project is untouched and can be started on again.
            let whole = contents(&w.dir(&first));
            drop(conv);
            assert!(!work.exists());
            assert_eq!((contents(&w.dir(&first)), converting_now()), (whole, None));
            let again = w.store.begin_conversion(&c).unwrap();
            assert_ne!(again.work_dir(), work);
        }

        #[test]
        fn two_projects_sharing_a_frames_file_by_link_keep_the_data_when_one_is_swapped() {
            let _one = one_conversion();
            let w = World::new();
            let (first, second) = (key_1(), key(SITE_A, TOUR_1, CH_2));
            w.save(&first);
            w.save(&second);
            let frames = frames_and_index(FRAMES).0;
            let linked = w.dir(&first).join(FRAMES_FILE);
            // One file under three names: the recording's own and both projects'. Written through one, read through another.
            let mut probe = frames.clone();
            probe[0] ^= 0xff;
            std::fs::OpenOptions::new().write(true).open(&linked).unwrap().write_all(&probe).unwrap();
            if std::fs::read(w.dir(&second).join(FRAMES_FILE)).unwrap() != probe {
                println!("skipped: the frames file could not be linked on this disk, so the projects share nothing");
            }
            std::fs::OpenOptions::new().write(true).open(&linked).unwrap().write_all(&frames).unwrap();
            let other = contents(&w.dir(&second));

            swap(&w, &first);
            assert!(video_form(&w.dir(&first)));
            assert_eq!(contents(&w.dir(&second)), other, "the other project still has every byte");
            assert_eq!(std::fs::read(w.tmp.path().join("rec").join(FRAMES_FILE)).unwrap(), frames, "and so has the recording they were saved from");
            assert_eq!(loaded(&w.store, &second).rec.frames.len(), FRAMES);
            // The other converts on its own.
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0).map(|c| c.key), Some(second.clone()));
            swap(&w, &second);
            assert!(video_form(&w.dir(&second)));
            assert_eq!(std::fs::read(w.tmp.path().join("rec").join(FRAMES_FILE)).unwrap(), frames);
        }

        #[test]
        fn a_stray_video_beside_a_frames_project_is_removed_at_the_next_start() {
            let w = World::new();
            for k in [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1), key(SITE_B, TOUR_1, CH_1)] {
                w.save(&k);
                std::fs::write(w.dir(&k).join(VIDEO_FILE), VIDEO).unwrap();
            }
            let (whole, no_frames, open_one, newer) = (w.dir(&key(SITE_A, TOUR_1, CH_1)), w.dir(&key(SITE_A, TOUR_1, CH_2)), w.dir(&key(SITE_A, TOUR_2, CH_1)), w.dir(&key(SITE_B, TOUR_1, CH_1)));
            std::fs::write(whole.join("project.json.0123456789abcdef.tmp"), b"cut off").unwrap();
            std::fs::write(whole.join("notes.txt"), b"mine").unwrap();
            // A frames project without its frames: the video beside it may be all there is, and stays.
            std::fs::remove_file(no_frames.join(FRAMES_FILE)).unwrap();
            let open = loaded(&w.store, &key(SITE_A, TOUR_2, CH_1));
            // A later version's project is never touched.
            let text = std::fs::read_to_string(newer.join(PROJECT_FILE)).unwrap().replacen("\"format\": 1", "\"format\": 3", 1);
            std::fs::write(newer.join(PROJECT_FILE), text).unwrap();
            let left_alone: Vec<_> = [&no_frames, &open_one, &newer].iter().map(|d| contents(d)).collect();
            let before = changes(&w.store);

            w.store.finish_conversions();
            assert_eq!(changes(&w.store), before + 1, "the sizes in the list changed");
            let mut names = tree(&whole);
            names.retain(|n| !n.starts_with("pictures") && !n.starts_with("logo-"));
            assert_eq!(names, vec![FRAMES_FILE.to_string(), "notes.txt".to_string(), PROJECT_FILE.to_string(), INDEX_FILE.to_string()]);
            assert_eq!([&no_frames, &open_one, &newer].iter().map(|d| contents(d)).collect::<Vec<_>>(), left_alone);
            assert_eq!(w.store.list().iter().find(|p| p.dir == whole).map(|p| (p.problem, p.form)), Some((None, Form::Frames)));
            loaded(&w.store, &key(SITE_A, TOUR_1, CH_1));
            // Once its editor is closed, the open one's stray video goes at the next start too.
            drop(open);
            w.store.finish_conversions();
            assert!(!open_one.join(VIDEO_FILE).exists() && open_one.join(FRAMES_FILE).is_file());
            assert_eq!([&no_frames, &newer].iter().map(|d| contents(d)).collect::<Vec<_>>(), [left_alone[0].clone(), left_alone[2].clone()]);
        }

        #[test]
        fn a_video_project_whose_video_is_not_whole_keeps_every_file_at_the_next_start() {
            let _one = one_conversion();
            let w = World::new();
            for k in [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2)] {
                w.save(&k);
            }
            // The state between the last two steps of a swap, and then the video is cut short by hand.
            for k in [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2)] {
                let dir = w.dir(&k);
                let (frames, index) = (std::fs::read(dir.join(FRAMES_FILE)).unwrap(), std::fs::read(dir.join(INDEX_FILE)).unwrap());
                swap(&w, &k);
                std::fs::write(dir.join(FRAMES_FILE), frames).unwrap();
                std::fs::write(dir.join(INDEX_FILE), index).unwrap();
            }
            let (cut, gone) = (w.dir(&key(SITE_A, TOUR_1, CH_1)), w.dir(&key(SITE_A, TOUR_1, CH_2)));
            std::fs::write(cut.join(VIDEO_FILE), &VIDEO[..10]).unwrap();
            std::fs::remove_file(gone.join(VIDEO_FILE)).unwrap();
            let whole = contents(w.store.root());
            w.store.finish_conversions();
            assert_eq!(contents(w.store.root()), whole, "the frames are never removed for a video that is not the one the project names");
            assert!(w.store.list().iter().all(|p| p.problem == Some(Problem::FramesMissing)));
        }

        #[test]
        fn a_folder_holding_only_a_video_and_no_project_file_is_removed_at_the_next_start() {
            let w = World::new();
            w.save(&key(SITE_B, TOUR_2, CH_2));
            let root = w.store.root().to_path_buf();
            let folder = |tour: &str, chapter: &str| {
                let dir = root.join(site_of(SITE_A)).join(tour).join(chapter);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(VIDEO_FILE), VIDEO).unwrap();
                dir
            };
            // What 0.2.7 leaves when it deletes a tour whose recording is kept as a video.
            let (left, also_left) = (folder(TOUR_1, CH_1), folder(TOUR_1, CH_2));
            // Folders that hold something more, or are not the store's to judge.
            let with_a_note = folder(TOUR_2, CH_1);
            std::fs::write(with_a_note.join("notes.txt"), b"mine").unwrap();
            let with_frames = folder(TOUR_2, CH_2);
            std::fs::write(with_frames.join(FRAMES_FILE), b"frames").unwrap();
            let with_a_folder = folder(TOUR_2, "ch_3");
            std::fs::create_dir(with_a_folder.join("mine")).unwrap();
            let misnamed = folder("Tour 9", CH_1);
            let too_high = root.join(site_of(SITE_A)).join("tour_4");
            std::fs::create_dir_all(&too_high).unwrap();
            std::fs::write(too_high.join(VIDEO_FILE), VIDEO).unwrap();
            let a_folder_named_so = root.join(site_of(SITE_A)).join("tour_5").join(CH_1);
            std::fs::create_dir_all(a_folder_named_so.join(VIDEO_FILE)).unwrap();
            // One file only, and it is not a video: somebody's note, and a frames file without its project.
            let (only_a_note, only_frames) = (folder("tour_6", CH_1), folder("tour_7", CH_1));
            std::fs::rename(only_a_note.join(VIDEO_FILE), only_a_note.join("notes.txt")).unwrap();
            std::fs::rename(only_frames.join(VIDEO_FILE), only_frames.join(FRAMES_FILE)).unwrap();
            let kept: Vec<PathBuf> = vec![with_a_note, with_frames, with_a_folder, misnamed, too_high, a_folder_named_so, only_a_note, only_frames, w.dir(&key(SITE_B, TOUR_2, CH_2))];
            let before: Vec<_> = kept.iter().map(|d| tree(d)).collect();
            let epoch = changes(&w.store);

            w.store.finish_conversions();
            assert!(!left.exists() && !also_left.exists());
            assert!(!root.join(site_of(SITE_A)).join(TOUR_1).exists(), "the tour's folder goes when it is empty");
            assert_eq!(kept.iter().map(|d| tree(d)).collect::<Vec<_>>(), before);
            assert_eq!(changes(&w.store), epoch, "none of them was ever listed");
            assert_eq!(w.store.list().len(), 1);
        }

        #[test]
        fn only_old_converting_folders_with_the_exact_name_are_swept() {
            let w = World::new();
            w.save(&key_1());
            let root = w.store.root().to_path_buf();
            let now = SystemTime::now();
            let started = since_1970(now);
            let cut_off = root.join(format!(".converting-{started}-0123456789abcdef"));
            std::fs::create_dir(&cut_off).unwrap();
            std::fs::write(cut_off.join(VIDEO_FILE), b"half").unwrap();
            let not_ours = [
                ".converting-".to_string(),
                ".converting-0123456789abcdef".to_string(),
                format!(".converting-{started}-0123456789abcde"),
                format!(".converting-{started}-0123456789abcdef0"),
                format!(".converting-{started}-0A1B2C3D4E5F6A7B"),
                format!(".converting--{started}-1123456789abcdef"),
                format!(".converting-x{started}-2123456789abcdef"),
                format!(".converting-{started}0000-3123456789abcdef"),
                format!("converting-{started}-0123456789abcdef"),
                format!(".converting-{started}-0123456789abcdef.bak"),
                format!(".staging-{started}-0123456789abcdef"),
                ".replaced-0123456789abcdef".to_string(),
            ];
            for name in &not_ours {
                std::fs::create_dir(root.join(name)).unwrap();
                std::fs::write(root.join(name).join("kept.txt"), b"x").unwrap();
            }
            std::fs::write(root.join(format!(".converting-{started}-fedcba9876543210")), b"a file").unwrap();
            std::fs::create_dir(root.join(site_of(SITE_A)).join(format!(".converting-{started}-0123456789abcdef"))).unwrap();
            let everything = tree(&root);

            // Not old enough yet: a conversion may still be writing it.
            w.store.sweep_converting(DAY);
            w.store.sweep_converting_at(DAY, now + 23 * HOUR);
            assert_eq!(tree(&root), everything);
            // Sweeping what a save left takes none of it, and the other way round.
            w.store.sweep_staging_at(Duration::ZERO, now + 25 * HOUR);
            assert!(cut_off.exists() && !root.join(format!(".staging-{started}-0123456789abcdef")).exists());
            let everything: Vec<String> = everything.into_iter().filter(|n| !n.starts_with(".staging-")).collect();
            // A day and an hour later the cut-off conversion goes, and only that.
            w.store.sweep_converting_at(DAY, now + 25 * HOUR);
            assert!(!cut_off.exists());
            let gone = format!(".converting-{started}-0123456789abcdef/");
            assert_eq!(tree(&root), everything.into_iter().filter(|n| !n.starts_with(&gone)).collect::<Vec<_>>());
            assert_eq!(changes(&w.store), 1, "sweeping is not a change to the list");
            loaded(&w.store, &key_1());
            Store::at(w.tmp.path().join("never made")).sweep_converting(Duration::ZERO);
            assert!(!w.tmp.path().join("never made").exists());
        }

        // ---- Opening ----

        #[test]
        fn a_video_project_expands_into_the_temporary_folder_and_loads_with_its_saved_edit() {
            let _one = one_conversion();
            let w = World::new();
            let saved = w.save(&key_1());
            let dir = w.dir(&key_1());
            assert_eq!(w.store.form_of(&key_1()), Some(Form::Frames));
            swap(&w, &key_1());
            assert_eq!((w.store.form_of(&key_1()), w.store.form_of(&key(SITE_A, TOUR_1, CH_2)), w.store.form_of(&key(SITE_A, "..", CH_1))), (Some(Form::Video), None, None));
            let whole = contents(&dir);
            // The temporary folder is made when it is not there.
            let temp = w.tmp.path().join("Mythicshot").join("edit-1-2");
            let asked = std::cell::RefCell::new(Vec::new());
            let l = w
                .store
                .load_with(
                    &key_1(),
                    &Expand {
                        codec: Some(&Fake::default()),
                        temp: &temp,
                        keep_going: &|done, of| {
                            asked.borrow_mut().push((done, of));
                            true
                        },
                    },
                )
                .unwrap();
            assert!(asked.borrow().iter().all(|(done, of)| done <= of && *of == FRAMES) && asked.borrow().contains(&(FRAMES - 1, FRAMES)), "{asked:?}");
            assert_eq!((l.info.form, l.info.convert, l.info.problem, l.info.saved_at, l.info.key.clone()), (Form::Video, ConvertState::Done, None, saved.info.saved_at, key_1()));
            // An ordinary recording of its own in the temporary folder, with the saved times.
            assert_eq!(tree(&temp), vec![FRAMES_FILE.to_string()]);
            assert_eq!((l.rec.width, l.rec.height, l.rec.end_ms, l.rec.frames.len()), (100, 80, 1000, FRAMES));
            assert_eq!(l.rec.frames.iter().map(|m| (m.t_ms, m.cursor)).collect::<Vec<_>>(), w.rec.frames.iter().map(|m| (m.t_ms, m.cursor)).collect::<Vec<_>>());
            assert_eq!(l.rec.frame(7).unwrap().as_raw()[..4], [7, 7, 7, 7], "picture 7 of the video is frame 7");
            assert_eq!(l.rec.expanded_from(), Some((dir.join(VIDEO_FILE).as_path(), VIDEO.len() as u64)));
            let mut want = w.edit.clone();
            want.tour_doc.logo_path = Some(dir.join(logo_name()));
            assert_same_edit(&l.restored.edit, &want);
            assert_same_items(&l.restored.items, &w.items);
            assert_eq!((l.restored.events.clone(), l.restored.title.as_str(), l.restored.dropped.len()), (mouse_only(&w.events), "Chapter one", 0));
            assert_eq!(contents(&dir), whole, "opening writes nothing into the project");
            // It is open like any project, until its guard goes.
            assert_eq!(why(open_with(&w.store, &key_1(), &Fake::default(), &w.tmp.path().join("second"))), LoadError::AlreadyOpen);
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), None);
            let Loaded { rec, open, .. } = l;
            // The editor deletes its recording as it deletes any: the temporary folder goes, the video stays.
            Arc::try_unwrap(rec).ok().unwrap().delete();
            assert!(!temp.exists());
            drop(open);
            assert_eq!(contents(&dir), whole);
            open_with(&w.store, &key_1(), &Fake::default(), &w.tmp.path().join("third")).unwrap();
        }

        #[test]
        fn without_a_codec_a_video_project_says_it_needs_ffmpeg_and_nothing_changes() {
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            swap(&w, &key_1());
            let whole = contents(w.tmp.path());
            let needs = LoadError::NeedsFfmpeg;
            assert_eq!(load_err(&w.store, &key_1()), needs);
            assert_eq!(why(w.store.load_with(&key_1(), &Expand { codec: None, temp: &w.tmp.path().join("edit"), keep_going: &go })), needs);
            // An ffmpeg that was found at start and cannot be run now is the same thing to the user.
            let gone = Fake { fails: Some(|| CodecError::Failed { exit: None }), ..Fake::default() };
            let not_started = Fake { fails: Some(|| CodecError::Io(io::Error::other("the program could not be started"))), ..Fake::default() };
            for codec in [&gone, &not_started] {
                assert_eq!(why(open_with(&w.store, &key_1(), codec, &w.tmp.path().join("edit"))), needs);
            }
            let _ = std::fs::remove_dir(w.tmp.path().join("edit"));
            assert_eq!(contents(w.tmp.path()), whole, "nothing was written, changed or removed anywhere");
            assert_eq!(w.store.list()[0].problem, None, "the project is not a damaged one");
            // With ffmpeg back, it opens.
            open_with(&w.store, &key_1(), &Fake::default(), &w.tmp.path().join("edit")).unwrap();
        }

        #[test]
        fn a_cancelled_or_failed_expansion_lets_go_of_the_project_and_leaves_the_temporary_folder_empty() {
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            swap(&w, &key_1());
            let dir = w.dir(&key_1());
            let whole = contents(&dir);
            let temp = w.tmp.path().join("edit");
            let empty = |what: &str| {
                assert!(temp.is_dir(), "{what}");
                assert_eq!(tree(&temp), Vec::<String>::new(), "{what}");
                assert_eq!(contents(&dir), whole, "{what}: the project is exactly as it was");
            };
            // Cancelled after a few frames.
            let stop = |done: usize, _: usize| done < 4;
            assert_eq!(why(w.store.load_with(&key_1(), &Expand { codec: Some(&Fake::default()), temp: &temp, keep_going: &stop })), LoadError::Cancelled);
            empty("cancelled");
            // The program says it cannot read the video; the video holds too few pictures; a bound passed.
            let damaged = LoadError::VideoDamaged;
            let unreadable = Fake { fails: Some(|| CodecError::Failed { exit: Some(1) }), ..Fake::default() };
            let short = Fake { short_by: 1, ..Fake::default() };
            let slow = Fake { fails: Some(|| CodecError::TimedOut { bound: mythicshot_recording::Bound::Stall }), ..Fake::default() };
            for (what, codec) in [("unreadable", &unreadable), ("short", &short), ("slow", &slow)] {
                assert_eq!(why(open_with(&w.store, &key_1(), codec, &temp)), damaged, "{what}");
                empty(what);
            }
            // No room is its own reason.
            assert_eq!(expand_failed(mythicshot_recording::ExpandError::NoRoom), LoadError::NoRoom);
            assert_eq!(expand_failed(mythicshot_recording::ExpandError::Io(no_room())), LoadError::NoRoom);
            // A temporary folder that already holds something is not written into.
            std::fs::write(temp.join("mine.txt"), b"mine").unwrap();
            assert!(matches!(why(open_with(&w.store, &key_1(), &Fake::default(), &temp)), LoadError::Damaged(_)));
            assert_eq!(tree(&temp), vec!["mine.txt".to_string()]);
            std::fs::remove_file(temp.join("mine.txt")).unwrap();
            // None of it marked or changed the project, and none left it open: it opens now.
            assert_eq!((w.store.list()[0].problem, w.store.list()[0].convert), (None, ConvertState::Done));
            open_with(&w.store, &key_1(), &Fake::default(), &temp).unwrap();
            // The four reasons are four outcomes of their own, and none of them is "damaged".
            let reasons = [not_expanded(NotExpanded::NeedsFfmpeg), not_expanded(NotExpanded::Cancelled), not_expanded(NotExpanded::NoRoom), not_expanded(NotExpanded::VideoDamaged)];
            assert_eq!(reasons, [LoadError::NeedsFfmpeg, LoadError::Cancelled, LoadError::NoRoom, LoadError::VideoDamaged]);
        }

        #[test]
        fn a_project_cannot_be_expanded_twice_at_once() {
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            swap(&w, &key_1());
            let second = std::sync::Mutex::new(None);
            let (store, tmp) = (&w.store, w.tmp.path());
            let codec = Fake {
                during: Some(Box::new(|| {
                    // While the first expansion decodes, a second Edit on the same project.
                    let again = open_with(store, &key_1(), &Fake::default(), &tmp.join("second"));
                    *second.lock().unwrap() = Some(again.map(|_| ()));
                    assert_eq!(store.remove_tour(SITE_A, TOUR_1), Ok(1), "a delete meanwhile only marks it");
                })),
                ..Fake::default()
            };
            let first = open_with(&w.store, &key_1(), &codec, &w.tmp.path().join("first")).unwrap();
            assert_eq!(second.lock().unwrap().take(), Some(Err(LoadError::AlreadyOpen)));
            assert!(!w.tmp.path().join("second").exists(), "the second never got as far as a folder");
            assert_eq!(first.rec.frames.len(), FRAMES);
            // Deleted while it was open: it goes whole, video and all, when its editor closes.
            assert!(first.info.dir.join(VIDEO_FILE).is_file());
            drop(first);
            assert_eq!(tree(w.store.root()), Vec::<String>::new());
        }

        // ---- Saving ----

        #[test]
        fn an_update_of_a_video_project_rewrites_only_the_project_file_and_keeps_the_video_and_its_form() {
            let _one = one_conversion();
            let w = World::new();
            let first = w.save(&key_1());
            let dir = w.dir(&key_1());
            swap(&w, &key_1());
            let before = contents(&dir);
            let was = project_file(&dir);
            let l = open_with(&w.store, &key_1(), &Fake::default(), &w.tmp.path().join("edit")).unwrap();
            let mut edit = l.restored.edit.clone();
            edit.trim = (2, 5);
            let snap = Snapshot { rec: &l.rec, title: "Chapter one, again", tour_title: "Customer onboarding", steps: 2, edit: &edit, events: &l.restored.events, items: &l.restored.items };
            let epoch = changes(&w.store);
            let saved = w.store.save(&key_1(), &snap).unwrap();
            assert_eq!(changes(&w.store), epoch + 1);
            assert_eq!((saved.info.form, saved.info.convert, saved.info.problem, saved.info.title.as_str()), (Form::Video, ConvertState::Done, None, "Chapter one, again"));
            assert!(saved.info.saved_at >= first.info.saved_at);
            let after = contents(&dir);
            assert_eq!(after.iter().filter(|(n, _)| n != PROJECT_FILE).collect::<Vec<_>>(), before.iter().filter(|(n, _)| n != PROJECT_FILE).collect::<Vec<_>>(), "the video, its index, the pictures and the logo are not written");
            let now = project_file(&dir);
            assert_eq!((now.format, now.store.clone(), now.frames_bytes), (2, was.store.clone(), was.frames_bytes));
            assert_eq!((now.title.as_str(), now.edit.trim), ("Chapter one, again", Some((2, 5))));
            assert_eq!(tree(w.store.root()).into_iter().filter(|n| n.starts_with('.')).collect::<Vec<_>>(), Vec::<String>::new(), "nothing was staged or replaced");
            // A save that cannot replace the project file leaves it as it was.
            let whole = contents(&dir);
            let refuse = |_: &Path, _: &Path| -> io::Result<()> { Err(io::Error::other("the rename was refused")) };
            assert_eq!(w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &real_frames, rename: &refuse }).unwrap_err(), "the project file could not be written.");
            assert_eq!(contents(&dir), whole);
            // A video that changed under the open project stops the save.
            std::fs::write(dir.join(VIDEO_FILE), &VIDEO[..20]).unwrap();
            let err = w.store.save(&key_1(), &snap).unwrap_err();
            assert!(reads_as_a_reason(&err, w.tmp.path()), "{err}");
            std::fs::write(dir.join(VIDEO_FILE), VIDEO).unwrap();
            assert_eq!(contents(&dir), whole);
            drop(l);
            let again = open_with(&w.store, &key_1(), &Fake::default(), &w.tmp.path().join("edit again")).unwrap();
            assert_eq!((again.restored.title.as_str(), again.restored.edit.trim), ("Chapter one, again", (2, 5)));
        }

        #[test]
        fn an_update_from_a_recording_expanded_from_another_video_is_refused() {
            let _one = one_conversion();
            let w = World::new();
            let (own, other) = (key_1(), key(SITE_A, TOUR_1, CH_2));
            for k in [&own, &other] {
                w.save(k);
                swap(&w, k);
            }
            let dir = w.dir(&own);
            let whole = contents(&dir);
            let open = open_with(&w.store, &own, &Fake::default(), &w.tmp.path().join("edit own")).unwrap();
            let elsewhere = open_with(&w.store, &other, &Fake::default(), &w.tmp.path().join("edit other")).unwrap();
            // The same times, pointers and size, expanded from the video of another project.
            assert!(elsewhere.rec.same_timing(&mythicshot_recording::open_video_index(&dir).unwrap()));
            fn snap<'a>(w: &'a World, rec: &'a Recording) -> Snapshot<'a> {
                Snapshot { rec, title: "Late", tour_title: "Customer onboarding", steps: 3, edit: &w.edit, events: &w.events, items: &w.items }
            }
            let err = w.store.save(&own, &snap(&w, &elsewhere.rec)).unwrap_err();
            assert!(err.contains("another recording") && reads_as_a_reason(&err, w.tmp.path()), "{err}");
            // A recording that was never expanded from a video, with the same times.
            let err = w.store.save(&own, &snap(&w, &w.rec)).unwrap_err();
            assert!(err.contains("another recording"), "{err}");
            // The project's own recording with other times is not its recording either.
            let shorter = recorded(w.tmp.path(), "other", 4);
            let edit = Edit::new(&shorter, Default::default());
            let err = w.store.save(&own, &Snapshot { rec: &shorter, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] }).unwrap_err();
            assert!(err.ends_with('.'), "{err}");
            assert_eq!(contents(&dir), whole);
            // Its own recording is taken.
            w.store.save(&own, &snap(&w, &open.rec)).unwrap();
        }

        #[test]
        fn a_new_project_saved_from_an_expanded_recording_is_born_in_video_form_and_shares_the_video() {
            let _one = one_conversion();
            let w = World::new();
            let (first, second) = (key_1(), key(SITE_A, TOUR_2, CH_1));
            w.save(&first);
            swap(&w, &first);
            let l = open_with(&w.store, &first, &Fake::default(), &w.tmp.path().join("edit")).unwrap();
            let snap = Snapshot { rec: &l.rec, title: "As a new tour", tour_title: "Another tour", steps: 3, edit: &w.edit, events: &w.events, items: &w.items };
            let saved = w.store.save(&second, &snap).unwrap();
            let dir = w.dir(&second);
            assert_eq!((saved.info.form, saved.info.convert, saved.info.problem, saved.info.bytes), (Form::Video, ConvertState::Done, None, size_of(&dir)));
            assert!(video_form(&dir), "{:?}", tree(&dir));
            assert_eq!(std::fs::read(dir.join(VIDEO_FILE)).unwrap(), VIDEO, "the video itself, never one made from decoded frames");
            let file = project_file(&dir);
            // What the frames took is carried over from the project the video comes from.
            assert_eq!((file.format, file.store.clone(), file.frames_bytes), (2, StoreInfo { form: SavedForm::Video, video_bytes: VIDEO.len() as u64, lossless_bytes: 245, convert: None }, 245));
            assert_eq!(w.store.list().iter().map(|p| (p.form, p.problem)).collect::<Vec<_>>(), vec![(Form::Video, None); 2]);
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), None, "neither is converted again");
            // Removing the first project takes its name for the video and not the second's data.
            drop(l);
            assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(1));
            assert_eq!(std::fs::read(dir.join(VIDEO_FILE)).unwrap(), VIDEO);
            let again = open_with(&w.store, &second, &Fake::default(), &w.tmp.path().join("edit second")).unwrap();
            assert_eq!((again.restored.title.as_str(), again.rec.frames.len()), ("As a new tour", FRAMES));
            // An expanded recording whose video is gone is kept as its frames, like any recording.
            let third = key(SITE_B, TOUR_1, CH_1);
            drop(again.open);
            assert_eq!(w.store.remove_tour(SITE_A, TOUR_2), Ok(1));
            let snap = Snapshot { rec: &again.rec, title: "Without its video", tour_title: "Another tour", steps: 3, edit: &w.edit, events: &w.events, items: &w.items };
            let saved = w.store.save(&third, &snap).unwrap();
            assert_eq!((saved.info.form, saved.info.convert, project_file(&w.dir(&third)).format), (Form::Frames, ConvertState::NotTried, 1));
            assert!(frames_form(&w.dir(&third)));
            assert_eq!(loaded(&w.store, &third).rec.frames.len(), FRAMES);
        }

        #[test]
        fn removing_a_tour_removes_its_video_projects_whole() {
            let _one = one_conversion();
            let w = World::new();
            let all = [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1), key(SITE_A, TOUR_2, CH_2), key(SITE_B, TOUR_1, CH_1)];
            for k in &all {
                w.save(k);
                swap(&w, k);
            }
            // One as a swap cut off before its last steps left it, with a temporary file of each kind.
            let cut_off = w.dir(&all[1]);
            let (frames, index) = frames_and_index(FRAMES);
            std::fs::write(cut_off.join(FRAMES_FILE), frames).unwrap();
            std::fs::write(cut_off.join(INDEX_FILE), index).unwrap();
            for stale in ["frames.mkv.4242-17.tmp", "frames.mkv.0123456789abcdef.tmp", "recording.json.4242-18.tmp", "project.json.0123456789abcdef.tmp"] {
                std::fs::write(cut_off.join(stale), b"cut off").unwrap();
            }
            std::fs::write(w.dir(&all[0]).join("notes.txt"), b"mine").unwrap();
            assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(2));
            assert_eq!(tree(&w.store.root().join(site_of(SITE_A)).join(TOUR_1)), vec![format!("{CH_1}/"), format!("{CH_1}/notes.txt")], "only what the store wrote is removed, and all of that");
            // One chapter by its folder; one that is open goes when its editor closes; one at the next start.
            assert_eq!(w.store.remove_project(&w.dir(&all[2])), Ok(Removed::Now));
            assert!(!w.dir(&all[2]).exists());
            let open = open_with(&w.store, &all[3], &Fake::default(), &w.tmp.path().join("edit")).unwrap();
            assert_eq!(w.store.remove_project(&w.dir(&all[3])), Ok(Removed::WhenItsEditorCloses));
            assert!(w.dir(&all[3]).join(VIDEO_FILE).is_file());
            drop(open);
            assert!(!w.store.root().join(site_of(SITE_A)).join(TOUR_2).exists());
            mark_removing(&w.dir(&all[4])).unwrap();
            w.store.finish_removals();
            assert!(!w.store.root().join(site_of(SITE_B)).exists());
            assert_eq!(w.store.list(), vec![]);
            // Replaced whole by a save from a new recording: the old video does not stay behind.
            w.save(&all[4]);
            swap(&w, &all[4]);
            w.save(&all[4]);
            assert!(frames_form(&w.dir(&all[4])), "{:?}", tree(&w.dir(&all[4])));
            assert_eq!(in_root(&w.store, REPLACED), Vec::<PathBuf>::new());
        }

        // ---- Candidates and marks ----

        #[test]
        fn the_oldest_unconverted_project_is_next() {
            let _one = one_conversion();
            let w = World::new();
            let (newest, oldest, middle) = (key(SITE_A, TOUR_1, CH_1), key(SITE_B, TOUR_2, CH_2), key(SITE_A, TOUR_2, CH_1));
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), None);
            for (k, by) in [(&newest, 0), (&oldest, -120_000), (&middle, -60_000)] {
                w.save(k);
                saved_later(&w.dir(k), by);
            }
            let c = w.store.next_candidate(TEST_CODEC, 0).unwrap();
            let file = project_file(&w.dir(&oldest));
            assert_eq!((c.key.clone(), c.dir.clone(), c.saved_at, c.frames_bytes), (oldest.clone(), w.dir(&oldest), file.saved_at, 245));
            // Asking changes nothing, and asks for the same one again.
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), Some(c.clone()));
            let conv = w.store.begin_conversion(&c).unwrap();
            std::fs::write(conv.video_path(), VIDEO).unwrap();
            assert_eq!(w.store.swap_to_video(&c, &conv, VIDEO.len() as u64), Ok(true));
            drop(conv);
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0).map(|c| c.key), Some(middle.clone()));
            swap(&w, &middle);
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0).map(|c| c.key), Some(newest.clone()));
            swap(&w, &newest);
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), None);
        }

        #[test]
        fn a_project_the_caller_skips_gives_way_to_the_next_oldest() {
            let w = World::new();
            let (newest, oldest, middle) = (key(SITE_A, TOUR_1, CH_1), key(SITE_B, TOUR_2, CH_2), key(SITE_A, TOUR_2, CH_1));
            for (k, by) in [(&newest, 0), (&oldest, -120_000), (&middle, -60_000)] {
                w.save(k);
                saved_later(&w.dir(k), by);
            }
            let whole = contents(w.store.root());
            let next = |skip: &dyn Fn(&ProjectKey, i64) -> bool| w.store.look(TEST_CODEC, 0, skip).next.map(|c| c.key);
            assert_eq!(next(&|_, _| false), Some(oldest.clone()));
            assert_eq!(w.store.look(TEST_CODEC, 0, &|_, _| false).next, w.store.next_candidate(TEST_CODEC, 0));
            assert_eq!(next(&|k, _| *k == oldest), Some(middle.clone()));
            assert_eq!(next(&|k, _| *k == oldest || *k == middle), Some(newest.clone()));
            assert_eq!(next(&|_, _| true), None);
            // It is asked with the time of saving the project file names.
            let at = project_file(&w.dir(&middle)).saved_at;
            assert_eq!(next(&|_, saved_at| saved_at != at), Some(middle.clone()));
            // The least size still counts, and nothing was changed by asking.
            assert_eq!(w.store.look(TEST_CODEC, u64::MAX, &|_, _| false), Look::default());
            assert_eq!(contents(w.store.root()), whole);
        }

        #[test]
        fn a_look_names_the_projects_only_an_editor_keeps_from_being_next() {
            let w = World::new();
            let (oldest, middle, noted, going) = (key(SITE_B, TOUR_2, CH_2), key(SITE_A, TOUR_2, CH_1), key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2));
            for (k, by) in [(&oldest, -120_000), (&middle, -60_000), (&noted, -30_000), (&going, 0)] {
                w.save(k);
                saved_later(&w.dir(k), by);
            }
            marked(&w.dir(&noted), MARK_FAILED, TEST_CODEC, env!("CARGO_PKG_VERSION"));
            let whole = contents(w.store.root());
            let look = |least: u64, skip: &dyn Fn(&ProjectKey, i64) -> bool| {
                let look = w.store.look(TEST_CODEC, least, skip);
                (look.next.map(|c| c.key), look.open)
            };
            assert_eq!(look(0, &|_, _| false), (Some(oldest.clone()), vec![]));
            // Open in an editor: not the next, and named. One that would not be next with no
            // editor either (it carries a mark, or is being removed) is not named.
            let (open, _noted, _going) = (loaded(&w.store, &oldest), loaded(&w.store, &noted), loaded(&w.store, &going));
            assert_eq!(contents(w.store.root()), whole, "asking and opening change nothing");
            mark_removing(&w.dir(&going)).unwrap();
            assert_eq!(look(0, &|_, _| false), (Some(middle.clone()), vec![oldest.clone()]));
            // Nor is one the caller skips, or one under the least size.
            assert_eq!(look(0, &|k, _| *k == oldest), (Some(middle.clone()), vec![]));
            assert_eq!(look(u64::MAX, &|_, _| false), (None, vec![]));
            // The editor closes: it is the next again.
            drop(open);
            assert_eq!(look(0, &|_, _| false), (Some(oldest), vec![]));
        }

        #[test]
        fn an_open_a_small_a_damaged_and_a_marked_project_are_never_candidates() {
            let w = World::new();
            let all = [key(SITE_A, TOUR_1, CH_1), key(SITE_A, TOUR_1, CH_2), key(SITE_A, TOUR_2, CH_1), key(SITE_A, TOUR_2, CH_2), key(SITE_B, TOUR_1, CH_1), key(SITE_B, TOUR_1, CH_2), key(SITE_B, TOUR_2, CH_1), key(SITE_B, TOUR_2, CH_2)];
            for k in &all {
                w.save(k);
            }
            let [_, failed, not_smaller, no_frames, bad_index, removing, newer, cut] = all.clone().map(|k| w.dir(&k));
            let _open = loaded(&w.store, &all[0]);
            marked(&failed, MARK_FAILED, TEST_CODEC, env!("CARGO_PKG_VERSION"));
            marked(&not_smaller, MARK_NOT_SMALLER, TEST_CODEC, env!("CARGO_PKG_VERSION"));
            std::fs::remove_file(no_frames.join(FRAMES_FILE)).unwrap();
            // An index that does not describe the frames file: the cheap check passes, opening does not.
            std::fs::write(bad_index.join(INDEX_FILE), frames_and_index(FRAMES - 1).1).unwrap();
            mark_removing(&removing).unwrap();
            let text = std::fs::read_to_string(newer.join(PROJECT_FILE)).unwrap().replacen("\"format\": 1", "\"format\": 3", 1);
            std::fs::write(newer.join(PROJECT_FILE), text).unwrap();
            let text = std::fs::read(cut.join(PROJECT_FILE)).unwrap();
            std::fs::write(cut.join(PROJECT_FILE), &text[..text.len() / 2]).unwrap();
            let whole = contents(w.store.root());
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), None);
            assert_eq!(contents(w.store.root()), whole);
            // A whole project that is too small to be worth it; one byte less asked for, and it is one.
            let small = key(SITE_A, "tour_small", CH_1);
            w.save(&small);
            assert_eq!(w.store.next_candidate(TEST_CODEC, 246), None);
            assert_eq!(w.store.next_candidate(TEST_CODEC, u64::MAX), None);
            assert_eq!(w.store.next_candidate(TEST_CODEC, 245).map(|c| c.key), Some(small));
        }

        #[test]
        fn a_mark_for_another_codec_or_app_version_is_tried_again() {
            let w = World::new();
            w.save(&key_1());
            let dir = w.dir(&key_1());
            let app = env!("CARGO_PKG_VERSION");
            for (state, codec, version, again) in [(MARK_FAILED, TEST_CODEC, app, false), (MARK_NOT_SMALLER, TEST_CODEC, app, false), (MARK_FAILED, "another-codec", app, true), (MARK_NOT_SMALLER, TEST_CODEC, "0.0.1", true), ("something else", TEST_CODEC, app, true)] {
                marked(&dir, state, codec, version);
                assert_eq!(w.store.next_candidate(TEST_CODEC, 0).is_some(), again, "{state} {codec} {version}");
                // And the other way round: what this codec was refused, another codec may try.
                assert!(w.store.next_candidate("a-codec-nobody-marked", 0).is_some());
            }
        }

        #[test]
        fn a_mark_changes_nothing_but_the_project_file() {
            let w = World::new();
            let saved = w.save(&key_1());
            let dir = w.dir(&key_1());
            let (before, was) = (contents(&dir), project_file(&dir));
            let c = w.store.next_candidate(TEST_CODEC, 0).unwrap();
            let epoch = changes(&w.store);
            assert_eq!(w.store.mark_conversion(&c, MARK_NOT_SMALLER, TEST_CODEC), Ok(true));
            assert_eq!(changes(&w.store), epoch + 1, "the pages read the list again");
            let after = contents(&dir);
            assert_eq!(after.iter().filter(|(n, _)| n != PROJECT_FILE).collect::<Vec<_>>(), before.iter().filter(|(n, _)| n != PROJECT_FILE).collect::<Vec<_>>());
            assert_eq!(after.len(), before.len(), "no temporary file is left");
            let mut now = project_file(&dir);
            let mark = now.store.convert.take().unwrap();
            assert_eq!((mark.state.as_str(), mark.codec.as_str(), mark.app.as_str()), (MARK_NOT_SMALLER, TEST_CODEC, env!("CARGO_PKG_VERSION")));
            assert!(mark.at >= saved.info.saved_at);
            assert_eq!(now, was, "the time of saving included: a mark is not a save");
            assert_eq!(now.format, 1);
            let listed = &w.store.list()[0];
            assert_eq!((listed.convert, listed.form, listed.problem, listed.saved_at), (ConvertState::NotSmaller, Form::Frames, None, saved.info.saved_at));
            assert_eq!(w.store.next_candidate(TEST_CODEC, 0), None);
            // It opens as before, an Update from its editor keeps the mark, and a save from a new recording drops it.
            let l = loaded(&w.store, &key_1());
            let snap = Snapshot { rec: &l.rec, title: "Again", tour_title: "Customer onboarding", steps: 3, edit: &l.restored.edit, events: &l.restored.events, items: &l.restored.items };
            assert_eq!(w.store.save(&key_1(), &snap).unwrap().info.convert, ConvertState::NotSmaller);
            assert_eq!(project_file(&dir).store.convert, Some(mark));
            drop(l);
            // A second mark replaces the first; a word that is no mark is refused and writes nothing.
            let c = Candidate { saved_at: project_file(&dir).saved_at, ..c };
            assert_eq!(w.store.mark_conversion(&c, MARK_FAILED, "another-codec"), Ok(true));
            assert_eq!(project_file(&dir).store.convert.map(|m| (m.state, m.codec)), Some((MARK_FAILED.to_string(), "another-codec".to_string())));
            let whole = contents(&dir);
            let err = w.store.mark_conversion(&c, "done", TEST_CODEC).unwrap_err();
            assert!(reads_as_a_reason(&err, w.tmp.path()), "{err}");
            assert_eq!(contents(&dir), whole);
            assert_eq!(w.save(&key_1()).info.convert, ConvertState::NotTried);
            assert_eq!(project_file(&dir).store, StoreInfo::default());
        }

        // ---- Hardening after review ----

        /// The project of `key_1` as a swap cut off after its commit leaves it: the project file
        /// names the video, and the frames and their index are still there.
        fn cut_off_after_the_commit(w: &World) -> PathBuf {
            let dir = w.dir(&key_1());
            let (frames, index) = (std::fs::read(dir.join(FRAMES_FILE)).unwrap(), std::fs::read(dir.join(INDEX_FILE)).unwrap());
            swap(w, &key_1());
            std::fs::write(dir.join(FRAMES_FILE), frames).unwrap();
            std::fs::write(dir.join(INDEX_FILE), index).unwrap();
            dir
        }

        #[test]
        fn a_video_taken_away_between_the_steps_of_a_swap_never_costs_the_frames() {
            let frames = frames_and_index(FRAMES);
            // After which rename an outside hand takes the video, and whether it removes it or cuts it short.
            for (after, cut_short) in [(1, false), (1, true), (2, false), (2, true), (3, false), (3, true)] {
                let _one = one_conversion();
                let w = World::new();
                w.save(&key_1());
                let dir = w.dir(&key_1());
                let whole = contents(&dir);
                let (c, conv) = converted(&w, &key_1());
                let count = std::cell::Cell::new(0);
                let rename = |from: &Path, to: &Path| {
                    count.set(count.get() + 1);
                    std::fs::rename(from, to)?;
                    if count.get() == after {
                        if cut_short { std::fs::write(dir.join(VIDEO_FILE), &VIDEO[..VIDEO.len() - 1]).unwrap() } else { std::fs::remove_file(dir.join(VIDEO_FILE)).unwrap() }
                    }
                    Ok(())
                };
                let result = w.store.swap_with(&c, &conv, VIDEO.len() as u64, &Steps { keep_frames: &real_frames, rename: &rename });
                drop(conv);
                let what = format!("after rename {after}, cut short: {cut_short}");
                // Whatever else: the frames are there, every byte.
                assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), frames.0, "{what}");
                if after == 1 {
                    // Before the commit: a refused step, and the project is as it was.
                    let err = result.unwrap_err();
                    assert!(reads_as_a_reason(&err, w.tmp.path()), "{what}: {err}");
                    assert_eq!(count.get(), 1, "{what}: the project file was not replaced");
                    assert_eq!(contents(&dir), whole, "{what}");
                    assert_eq!(lists_and_loads("taken", &w.store, w.tmp.path(), &what), Form::Frames);
                } else {
                    // After it: the swap is made, the frames are kept, and nothing later removes them.
                    assert_eq!(result, Ok(true), "{what}");
                    assert_eq!(count.get(), after, "{what}: no step that leans on the video is taken after it changed");
                    assert_eq!(w.store.list()[0].problem, Some(Problem::FramesMissing), "{what}");
                    let left = contents(&dir);
                    w.store.finish_conversions();
                    w.store.sweep_converting(Duration::ZERO);
                    assert_eq!(contents(&dir), left, "{what}: the next start removes nothing");
                    // Taken before the index was replaced: the frames and their index are still a whole recording.
                    assert_eq!(Recording::open_saved(&dir).is_ok(), after == 2, "{what}");
                }
            }
        }

        #[test]
        fn the_next_start_keeps_the_frames_of_a_video_project_that_would_not_open_as_one() {
            // The index cannot be read; it names another video size; it is not there.
            for case in ["unreadable", "another size", "newer", "gone"] {
                let _one = one_conversion();
                let w = World::new();
                w.save(&key_1());
                let dir = cut_off_after_the_commit(&w);
                match case {
                    "unreadable" => std::fs::write(dir.join(INDEX_FILE), b"{\"format\": 2, \"width\"").unwrap(),
                    "another size" => Recording::open_saved(&dir).unwrap().write_video_index(&dir, VIDEO.len() as u64 + 1).unwrap(),
                    "newer" => std::fs::write(dir.join(INDEX_FILE), b"{\"format\": 3}").unwrap(),
                    _ => std::fs::remove_file(dir.join(INDEX_FILE)).unwrap(),
                }
                let (whole, epoch) = (contents(&dir), changes(&w.store));
                w.store.finish_conversions();
                assert_eq!(contents(&dir), whole, "{case}: nothing is removed from a project that would not open");
                assert_eq!(changes(&w.store), epoch, "{case}");
                assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), frames_and_index(FRAMES).0, "{case}");
                assert!(open_with(&w.store, &key_1(), &Fake::default(), &w.tmp.path().join("edit")).is_err(), "{case}: it does not open, which is why");
            }
            // The same state with its index whole is finished: the test above is not passing for another reason.
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            let dir = cut_off_after_the_commit(&w);
            w.store.finish_conversions();
            assert!(video_form(&dir));
            // An index of the frames form whose frames are damaged still names the times: the project opens, so the frames go.
            let w = World::new();
            w.save(&key_1());
            let dir = cut_off_after_the_commit(&w);
            std::fs::write(dir.join(FRAMES_FILE), b"cut").unwrap();
            w.store.finish_conversions();
            assert!(!dir.join(FRAMES_FILE).exists() && form_of(&dir) == Ok(Form::Frames));
            assert_eq!(lists_and_loads("damaged frames", &w.store, w.tmp.path(), "after"), Form::Video);
        }

        #[test]
        fn a_conversion_of_one_project_is_never_swapped_into_another() {
            let _one = one_conversion();
            let w = World::new();
            let (first, second) = (key_1(), key(SITE_A, TOUR_1, CH_2));
            w.save(&first);
            w.save(&second);
            let (_, conv) = converted(&w, &first);
            // The second project is whole, unchanged, not open: everything but the conversion is its own.
            let other = candidate(&second, &w.dir(&second));
            let whole = contents(w.store.root());
            assert_eq!(w.store.swap_to_video(&other, &conv, VIDEO.len() as u64), Ok(false));
            assert_eq!(contents(w.store.root()), whole, "nothing was touched, the video in the conversion's folder included");
            assert!(frames_form(&w.dir(&second)) && frames_form(&w.dir(&first)));
        }

        #[test]
        fn a_save_over_a_project_that_is_being_converted_waits_for_it_once_and_not_under_the_lock() {
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            let (_c, conv) = converted(&w, &key_1());
            let other = recorded(w.tmp.path(), "other", 4);
            let edit = Edit::new(&other, Default::default());
            let started = Instant::now();
            // The conversion never lets go here: the save waits its two seconds once, outside the lock.
            w.store.save(&key_1(), &Snapshot { rec: &other, title: "Shorter", tour_title: "Customer onboarding", steps: 1, edit: &edit, events: &[], items: &[] }).unwrap();
            let took = started.elapsed();
            assert!(conv.cancelled());
            assert!(took >= LET_GO && took < LET_GO * 2 - Duration::from_millis(400), "{took:?}");
        }

        #[cfg(windows)]
        #[test]
        fn the_folder_of_the_running_conversion_is_never_swept_however_the_projects_folder_is_spelled() {
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            let (_c, conv) = converted(&w, &key_1());
            // The same folder, spelled in capitals: the system finds it, and the paths are not equal.
            let shouted = PathBuf::from(w.store.root().to_string_lossy().to_uppercase());
            assert!(shouted != w.store.root() && shouted.is_dir());
            let other = own_store(shouted);
            other.sweep_converting_at(Duration::ZERO, SystemTime::now() + FAR);
            w.store.sweep_converting_at(Duration::ZERO, SystemTime::now() + FAR);
            assert_eq!(std::fs::read(conv.video_path()).unwrap(), VIDEO, "the running conversion's folder is whole");
            // Once it is given up, the name is nobody's: a cut-off folder of that name would go.
            let work = conv.work_dir().to_path_buf();
            drop(conv);
            std::fs::create_dir(&work).unwrap();
            other.sweep_converting_at(Duration::ZERO, SystemTime::now() + FAR);
            assert!(!work.exists());
        }

        #[test]
        fn a_temporary_project_file_a_cut_off_mark_left_is_cleared_at_the_next_start() {
            let w = World::new();
            let (marked_one, open_one) = (key_1(), key(SITE_A, TOUR_1, CH_2));
            w.save(&marked_one);
            w.save(&open_one);
            let (dir, open_dir) = (w.dir(&marked_one), w.dir(&open_one));
            let whole = contents(&dir);
            // The app died between writing the new project file and renaming it: no video anywhere.
            for d in [&dir, &open_dir] {
                std::fs::write(d.join("project.json.0123456789abcdef.tmp"), b"{\"format\": 1, \"store\"").unwrap();
            }
            std::fs::write(dir.join("notes.txt.0123456789abcdef.tmp"), b"mine").unwrap();
            let open = loaded(&w.store, &open_one);
            let epoch = changes(&w.store);
            w.store.finish_conversions();
            let mut want = whole.clone();
            want.push(("notes.txt.0123456789abcdef.tmp".to_string(), b"mine".to_vec()));
            want.sort();
            assert_eq!(contents(&dir), want, "the store's own temporary file is gone, and nothing else");
            assert_eq!(changes(&w.store), epoch + 1, "the size in the list changed");
            assert!(open_dir.join("project.json.0123456789abcdef.tmp").is_file(), "an open project is left alone");
            drop(open);
            w.store.finish_conversions();
            assert!(!open_dir.join("project.json.0123456789abcdef.tmp").exists());
            // Nothing left to clear: nothing changes.
            let epoch = changes(&w.store);
            w.store.finish_conversions();
            assert_eq!(changes(&w.store), epoch);
            loaded(&w.store, &marked_one);
        }

        #[cfg(windows)]
        #[test]
        fn a_fresh_video_another_program_holds_for_a_moment_is_still_swapped_in() {
            use std::os::windows::fs::OpenOptionsExt;
            let _one = one_conversion();
            let w = World::new();
            w.save(&key_1());
            let (c, conv) = converted(&w, &key_1());
            // A scanner reads the new video and lets nobody else open it, for a fifth of a second.
            let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(conv.video_path()).unwrap();
            let scanner = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                drop(held);
            });
            assert_eq!(w.store.swap_to_video(&c, &conv, VIDEO.len() as u64), Ok(true));
            scanner.join().unwrap();
            assert!(video_form(&w.dir(&key_1())));
        }
    }
}
