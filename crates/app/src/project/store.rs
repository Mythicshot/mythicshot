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
//! At start-up, in this order: [`Store::recover_replaced`], [`Store::finish_removals`],
//! [`Store::sweep_staging`].

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use mythicshot_recording::{OpenError, Recording, SaveError, SaveStep};
use sha2::Digest;

use super::state::{self, Blob, MAX_PROJECT_JSON_BYTES, ParseError, ProjectFile, ProjectKey, Restored, Snapshot};

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

/// The projects folder of this computer's user.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    /// [`epoch`]'s counter; a test gives each store its own.
    epoch: &'static AtomicU64,
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
    pub dir: PathBuf,
    /// Why the project cannot be opened, as far as the cheap check can tell.
    pub problem: Option<Problem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The project file cannot be read, or belongs to another tour or chapter.
    Damaged,
    /// The project was saved by a newer Mythicshot.
    Newer,
    /// The frames file or its index is not there, or the frames file is not as long as when it was saved.
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
}

/// What [`Store::remove_project`] did.
// Asked for by the saved-recordings list of Preferences; until that is built, only by the tests.
#[cfg_attr(not(test), allow(dead_code))]
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
    /// Puts the recording's frames file and index into a folder; the frames file's size.
    keep_frames: &'a dyn Fn(&Recording, &Path) -> Result<u64, SaveError>,
    /// Every rename of a save: a staging folder into place, the project it replaces out of the
    /// way and back, and the new `project.json` over the old one.
    rename: &'a dyn Fn(&Path, &Path) -> io::Result<()>,
}

/// A counter bumped by every save and every removal in this process, so a page knows when to
/// read the list again.
pub fn epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

static EPOCH: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn at(root: PathBuf) -> Store {
        Store { root, epoch: &EPOCH }
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

    // Only the tests ask: the app opens `default_root` itself.
    #[cfg_attr(not(test), allow(dead_code))]
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
        self.save_with(key, snap, &Steps { keep_frames: &|rec, dir| rec.save_into_with(dir, keep_going), rename: &|from, to| patiently(|| std::fs::rename(from, to)) })
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
        let info = ProjectInfo { key: key.clone(), title: file.title, tour_title: file.tour_title, saved_at: file.saved_at, bytes: bytes_in(&dir), dir, problem: None };
        Ok(Saved { info, left_out })
    }

    /// Build the whole project in a staging folder and rename it into place. A project already
    /// there, which no editor has open, is moved out of the way first and put back if the new one
    /// cannot take its place.
    fn save_whole(&self, dir: &Path, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<(ProjectFile, Vec<String>), String> {
        if matches!(read_project(dir), Err(Unreadable::Newer)) {
            return Err("a newer version of Mythicshot saved this tour's recording, so it is left as it is.".into());
        }
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
                let in_place = read_info(&dir, &site_of(&key.server), &key.tour_id, &key.chapter_id, Some(&key.server));
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
        sorted(self.chapter_folders().into_iter().filter_map(|(dir, site, tour, chapter)| read_info(&dir, &site, &tour, &chapter, None)).collect())
    }

    /// Every project of one site, read in one walk. `server` is matched exactly.
    pub fn for_site(&self, server: &str) -> Vec<ProjectInfo> {
        let site = site_of(server);
        let mut folders = Vec::new();
        chapters_of_site(&self.root.join(&site), &site, &mut folders);
        sorted(folders.into_iter().filter_map(|(dir, site, tour, chapter)| read_info(&dir, &site, &tour, &chapter, Some(server))).collect())
    }

    /// The projects of one tour on one site: one per saved chapter. `server` is matched exactly.
    // Only the tests ask for one tour: the Tours page reads a whole site at once with `for_site`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn for_tour(&self, server: &str, tour_id: &str) -> Vec<ProjectInfo> {
        if !is_folder_id(tour_id) {
            return Vec::new();
        }
        let site = site_of(server);
        sorted(folders_in(&self.root.join(&site).join(tour_id)).into_iter().filter(|(chapter, _)| is_folder_id(chapter)).filter_map(|(chapter, dir)| read_info(&dir, &site, tour_id, &chapter, Some(server))).collect())
    }

    pub fn totals(&self) -> Totals {
        let list = self.list();
        Totals { count: list.len(), bytes: list.iter().map(|p| p.bytes).sum() }
    }

    /// Open a project for an editor. Nothing is changed or removed, whatever is found.
    pub fn load(&self, key: &ProjectKey) -> Result<Loaded, LoadError> {
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
            OpenGuard::register(&dir, self.epoch).ok_or(LoadError::AlreadyOpen)?
        };
        let file = read_project(&dir).map_err(|e| match e {
            Unreadable::Newer => LoadError::Newer,
            Unreadable::NotThere => LoadError::NotSaved,
            Unreadable::Damaged(why) => LoadError::Damaged(why),
        })?;
        if (file.server.as_str(), file.tour_id.as_str(), file.chapter_id.as_str()) != (key.server.as_str(), key.tour_id.as_str(), key.chapter_id.as_str()) {
            return Err(LoadError::Damaged("the project file belongs to another tour or chapter".into()));
        }
        let rec = Recording::open_saved(&dir).map_err(|e| match e {
            OpenError::Missing => LoadError::Damaged("the recording's frames are not there".into()),
            OpenError::Damaged(why) => LoadError::Damaged(why),
            OpenError::Newer(_) => LoadError::Newer,
        })?;
        if !frames_are(&dir, file.frames_bytes) {
            return Err(LoadError::Damaged("the frames file is not the one the project was saved with".into()));
        }
        // Pictures are read from the project's own folder and from nowhere a link points to.
        if std::fs::symlink_metadata(dir.join(PICTURES)).is_ok() && !is_real_folder(&dir.join(PICTURES)) {
            return Err(LoadError::Damaged("the project's pictures folder is not a folder".into()));
        }
        let info = read_info(&dir, &site_of(&key.server), &key.tour_id, &key.chapter_id, Some(&key.server)).ok_or(LoadError::NotSaved)?;
        let restored = state::from_file(file, &dir, &rec);
        Ok(Loaded { info, rec: Arc::new(rec), restored, open })
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
    // Asked for by the saved-recordings list of Preferences; until that is built, only by the tests.
    #[cfg_attr(not(test), allow(dead_code))]
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
    let kept = Recording::open_saved(dir).map_err(|_| "the saved recording's frames can no longer be read.".to_string())?;
    if !kept.same_frames(snap.rec) {
        return Err("this tour's saved recording is open in an editor and belongs to another recording.".into());
    }
    let frames_bytes = std::fs::metadata(dir.join(FRAMES_FILE)).map_err(|e| reason("the saved recording's frames can no longer be read", &e))?.len();
    let (file, blobs, left_out) = state::to_file(key, snap, now_ms(), frames_bytes, state::StoreInfo::default());
    let text = project_text(&file)?;

    let _changing = changing();
    if !(is_project(dir) && is_open(dir)) {
        return Ok(None);
    }
    // Closed, replaced and opened again meanwhile: these are no longer the frames that were checked.
    if !frames_are(dir, frames_bytes) {
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
    let frames_bytes = (steps.keep_frames)(snap.rec, staging).map_err(frames_reason)?;
    let (file, blobs, left_out) = state::to_file(key, snap, now_ms(), frames_bytes, state::StoreInfo::default());
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
    let known = |n: &str| matches!(n, PROJECT_FILE | INDEX_FILE | FRAMES_FILE) || state::is_logo_name(n);
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
    files_in(dir).into_iter().chain(pictures_in(dir)).filter_map(|(_, path)| std::fs::symlink_metadata(path).ok()).map(|m| m.len()).sum()
}

/// Whether the frames file is a file of this many bytes.
fn frames_are(dir: &Path, bytes: u64) -> bool {
    std::fs::symlink_metadata(dir.join(FRAMES_FILE)).is_ok_and(|m| m.is_file() && m.len() == bytes)
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
    state::parse(&project_file_text(dir)?).map_err(|e| match e {
        ParseError::Newer(_) => Unreadable::Newer,
        ParseError::Damaged(why) => Unreadable::Damaged(why),
    })
}

/// What the list shows for the project in `dir`, the folder `<site>/<tour>/<chapter>`; None when
/// the folder holds no project file, or is being removed. The cheap check: the project file
/// parses, its address, tour and chapter are the folder's, and the frames file is there at the
/// size it was saved with.
fn read_info(dir: &Path, site: &str, tour: &str, chapter: &str, server: Option<&str>) -> Option<ProjectInfo> {
    if is_removing(dir) {
        return None;
    }
    let text = match project_file_text(dir) {
        Err(Unreadable::NotThere) => return None,
        other => other.ok(),
    };
    let parsed = text.as_deref().map(state::parse);
    // What a row shows is read even from a file that cannot be opened, when it is there to read.
    let loose: Option<serde_json::Value> = match &parsed {
        Some(Err(_)) => text.as_deref().and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok()),
        _ => None,
    };
    let word = |name: &str| loose.as_ref().and_then(|v| v.get(name)).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let loose_saved_at = loose.as_ref().and_then(|v| v.get("saved_at")).and_then(|v| v.as_i64()).unwrap_or(0);
    let (named_server, title, tour_title, saved_at, problem) = match &parsed {
        Some(Ok(file)) => {
            let belongs = site_of(&file.server) == site && file.tour_id == tour && file.chapter_id == chapter;
            let problem = if !belongs {
                Some(Problem::Damaged)
            } else if !frames_are(dir, file.frames_bytes) || !dir.join(INDEX_FILE).is_file() {
                Some(Problem::FramesMissing)
            } else {
                None
            };
            (file.server.clone(), file.title.clone(), file.tour_title.clone(), file.saved_at, problem)
        }
        Some(Err(ParseError::Newer(_))) => (word("server"), word("title"), word("tour_title"), loose_saved_at, Some(Problem::Newer)),
        _ => (word("server"), word("title"), word("tour_title"), loose_saved_at, Some(Problem::Damaged)),
    };
    let server = if site_of(&named_server) == site { named_server } else { server.unwrap_or("").to_string() };
    Some(ProjectInfo { key: ProjectKey { server, tour_id: tour.to_string(), chapter_id: chapter.to_string() }, title, tour_title, saved_at, bytes: bytes_in(dir), dir: dir.to_path_buf(), problem })
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
    let (started, random) = name.strip_prefix(STAGING)?.split_once('-')?;
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
        let stopped = |_: &Recording, dir: &Path| -> Result<u64, SaveError> {
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
        Store { root, epoch: Box::leak(Box::new(AtomicU64::new(0))) }
    }

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

    fn real_frames(rec: &Recording, dir: &Path) -> Result<u64, SaveError> {
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
        let full_disk = |_: &Recording, dir: &Path| -> Result<u64, SaveError> {
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
            let kinds: [(&str, fn(&Path, &Path) -> bool); 2] = [("junction", junction), ("folder link", folder_link)];
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
}
