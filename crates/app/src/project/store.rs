//! Where saved projects live, and how they are saved, listed, opened and removed.
//!
//! A project is the folder `<root>/<site>/<tour id>/<chapter id>/`: `project.json`, `recording.json`,
//! `frames.zst`, the cover logo and `pictures/`. `<site>` is the first 16 hex characters of the
//! SHA-256 of the site's address. A folder is a project only when it holds `project.json`, which is
//! written last when a project is saved and removed first when one is removed.
//!
//! A first save is built in `<root>/.staging-<random>/` and renamed into place. A project that is
//! open in an editor has its frames file open and cannot be renamed, so a save over it writes the
//! new pictures beside the old ones and then replaces `project.json` in one rename.
//!
//! Only files the store itself writes are ever removed: a file somebody else put into a project
//! folder stays where it is.

use std::collections::{BTreeSet, HashMap};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use mythicshot_recording::{OpenError, Recording};
use sha2::Digest;

use super::state::{self, Blob, ItemEntry, MAX_PROJECT_JSON_BYTES, ParseError, ProjectFile, ProjectKey, Restored, SavedShape, Snapshot};

const PROJECT_FILE: &str = "project.json";
const INDEX_FILE: &str = "recording.json";
const FRAMES_FILE: &str = "frames.zst";
const PICTURES: &str = "pictures";
/// A save in progress, and a folder moved out of a new save's way, is `<root>/.staging-<16 hex>`.
const STAGING: &str = ".staging-";
const RANDOM_CHARS: usize = 16;
/// The longest tour or chapter id that is used as a folder name.
const ID_MAX_CHARS: usize = 64;

/// How often a rename or a removal is tried while another program (a scanner, a sync) has the
/// file open: half a second in all.
const BUSY_TRIES: u32 = 5;
const BUSY_WAIT: Duration = Duration::from_millis(120);

/// The projects folder of this computer's user.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
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

/// Registers a project as open until dropped. A project whose tour was removed while it was open
/// is removed when its guard is dropped.
#[derive(Debug)]
pub struct OpenGuard {
    dir: PathBuf,
    name: PathBuf,
}

/// The steps of a save that a test makes fail.
struct Steps<'a> {
    /// Puts the recording's frames file and index into a folder; the frames file's size.
    keep_frames: &'a dyn Fn(&Recording, &Path) -> Result<u64, String>,
    /// The rename that makes a save count: a staging folder into place, or the new `project.json`
    /// over the old one.
    rename: &'a dyn Fn(&Path, &Path) -> io::Result<()>,
}

/// A counter bumped by every save and every removal in this process, so a page knows when to
/// read the list again.
pub fn epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

static EPOCH: AtomicU64 = AtomicU64::new(0);

fn bump() {
    EPOCH.fetch_add(1, Ordering::SeqCst);
}

impl Store {
    pub fn at(root: PathBuf) -> Store {
        Store { root }
    }

    /// Where projects live for this user: beside the app's local data, which does not roam. A
    /// debug build with `MYTHICSHOT_CONFIG_DIR` set uses that folder, as the settings file does.
    pub fn default_root() -> PathBuf {
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("MYTHICSHOT_CONFIG_DIR") {
            return PathBuf::from(dir).join("projects");
        }
        directories::ProjectDirs::from("", "", "Mythicshot").map(|d| d.data_local_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("projects")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Save the project for `key`. The error is a sentence that names no folder.
    pub fn save(&self, key: &ProjectKey, snap: &Snapshot<'_>) -> Result<Saved, String> {
        self.save_with(key, snap, &Steps { keep_frames: &|rec, dir| rec.save_into(dir), rename: &|from, to| patiently(|| std::fs::rename(from, to)) })
    }

    fn save_with(&self, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<Saved, String> {
        let dir = self.project_dir(key).ok_or("this tour's id cannot be used as a folder name.")?;
        let left_out = {
            let changing = changing();
            if dir.join(PROJECT_FILE).is_file() && is_open(&dir) {
                // Quick: a few pictures and one small file, and the editor that has it open waits for it.
                save_in_place(&dir, key, snap, steps)?
            } else {
                // The frames may have to be copied, which can take long: nothing else waits for that.
                drop(changing);
                self.save_whole(&dir, key, snap, steps)?
            }
        };
        bump();
        let info = read_info(&dir, &site_of(&key.server), &key.tour_id, &key.chapter_id, Some(&key.server)).ok_or("the project could not be read back after it was saved.")?;
        Ok(Saved { info, left_out })
    }

    /// Build the whole project in a staging folder and rename it into place. A project already
    /// there, which no editor has open, is moved out of the way first and put back if the new one
    /// cannot take its place.
    fn save_whole(&self, dir: &Path, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<Vec<String>, String> {
        if matches!(read_project(dir), Err(Unreadable::Newer)) {
            return Err("a newer version of Mythicshot saved this tour's recording, so it is left as it is.".into());
        }
        std::fs::create_dir_all(&self.root).map_err(|e| sentence("the folder for saved recordings could not be made", e))?;
        let staging = self.root.join(format!("{STAGING}{}", random_name()));
        std::fs::create_dir(&staging).map_err(|e| sentence("the folder for saved recordings could not be written", e))?;
        let result = fill(&staging, key, snap, steps).and_then(|left_out| {
            let _changing = changing();
            self.place(&staging, dir, steps).map(|()| left_out)
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
            std::fs::create_dir_all(parent).map_err(|e| sentence("the folder for saved recordings could not be written", e))?;
        }
        let aside = match std::fs::symlink_metadata(dir) {
            Ok(_) => {
                let aside = self.root.join(format!("{STAGING}{}", random_name()));
                patiently(|| std::fs::rename(dir, &aside)).map_err(|e| sentence("this tour's saved recording is in use and could not be replaced", e))?;
                Some(aside)
            }
            Err(_) => None,
        };
        if let Err(e) = (steps.rename)(staging, dir) {
            match &aside {
                Some(aside) => {
                    let _ = patiently(|| std::fs::rename(aside, dir));
                }
                // The folders made for it, when nothing else is in them.
                None => parents.iter().for_each(|parent| {
                    let _ = std::fs::remove_dir(parent);
                }),
            }
            return Err(sentence("the recording could not be put into the folder for saved recordings", e));
        }
        if let Some(aside) = &aside {
            // The old project's own files go; a file somebody else had put beside them moves along.
            let _ = remove_files(aside);
            for entry in std::fs::read_dir(aside).into_iter().flatten().flatten() {
                let to = dir.join(entry.file_name());
                if std::fs::symlink_metadata(&to).is_err() {
                    let _ = std::fs::rename(entry.path(), to);
                }
            }
            let _ = std::fs::remove_dir(aside);
        }
        Ok(())
    }

    /// Every project, of every address.
    pub fn list(&self) -> Vec<ProjectInfo> {
        let mut out = Vec::new();
        for (site, site_dir) in folders_in(&self.root) {
            if !is_random_name(&site) {
                continue;
            }
            for (tour, tour_dir) in folders_in(&site_dir) {
                if !is_folder_id(&tour) {
                    continue;
                }
                for (chapter, dir) in folders_in(&tour_dir) {
                    if is_folder_id(&chapter) {
                        out.extend(read_info(&dir, &site, &tour, &chapter, None));
                    }
                }
            }
        }
        out.sort_by(|a, b| a.dir.cmp(&b.dir));
        out
    }

    /// The projects of one tour on one site: one per saved chapter. `server` is matched exactly.
    pub fn for_tour(&self, server: &str, tour_id: &str) -> Vec<ProjectInfo> {
        if !is_folder_id(tour_id) {
            return Vec::new();
        }
        let site = site_of(server);
        let mut out: Vec<ProjectInfo> = folders_in(&self.root.join(&site).join(tour_id)).into_iter().filter(|(chapter, _)| is_folder_id(chapter)).filter_map(|(chapter, dir)| read_info(&dir, &site, tour_id, &chapter, Some(server))).collect();
        out.sort_by(|a, b| a.dir.cmp(&b.dir));
        out
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
            if !dir.join(PROJECT_FILE).is_file() {
                return Err(LoadError::NotSaved);
            }
            OpenGuard::register(&dir).ok_or(LoadError::AlreadyOpen)?
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
        let info = read_info(&dir, &site_of(&key.server), &key.tour_id, &key.chapter_id, Some(&key.server)).ok_or(LoadError::NotSaved)?;
        let restored = state::from_file(file, &dir, &rec);
        Ok(Loaded { info, rec: Arc::new(rec), restored, open })
    }

    /// Remove the projects of one tour on one site, and say how many. One that is open in an
    /// editor is counted and removed when that editor closes it. Only files the store wrote are
    /// removed; a folder that still holds something else is left with it.
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
            let is_project = dir.join(PROJECT_FILE).is_file();
            if remove_when_closed(&dir) {
                removed += usize::from(is_project);
                continue;
            }
            touched = true;
            match remove_files(&dir) {
                Ok(()) => removed += usize::from(is_project),
                Err(e) => failed = failed.or(Some(e)),
            }
        }
        // Folders the store made, when nothing is left in them.
        let _ = std::fs::remove_dir(&tour_dir);
        let _ = std::fs::remove_dir(&site_dir);
        if touched {
            bump();
        }
        match failed {
            Some(e) => Err(sentence("a saved recording of this tour could not be removed", e)),
            None => Ok(removed),
        }
    }

    /// Remove what a save that was cut off left behind: real folders directly in the projects
    /// folder named `.staging-` and exactly the random suffix, not changed for `age`.
    pub fn sweep_staging(&self, age: Duration) {
        self.sweep_staging_at(age, SystemTime::now());
    }

    fn sweep_staging_at(&self, age: Duration, now: SystemTime) {
        let _changing = changing();
        for (name, dir) in folders_in(&self.root) {
            let ours = name.strip_prefix(STAGING).is_some_and(is_random_name);
            let changed = std::fs::symlink_metadata(&dir).and_then(|m| m.modified());
            if ours && changed.is_ok_and(|at| now.duration_since(at).is_ok_and(|since| since >= age)) {
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
    fn register(dir: &Path) -> Option<OpenGuard> {
        let name = registry_name(dir);
        let mut open = registry();
        if open.contains_key(&name) {
            return None;
        }
        open.insert(name.clone(), false);
        Some(OpenGuard { dir: dir.to_path_buf(), name })
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        let _changing = changing();
        let remove = registry().remove(&self.name).unwrap_or(false);
        if remove {
            let _ = remove_files(&self.dir);
            for parent in self.dir.ancestors().skip(1).take(2) {
                let _ = std::fs::remove_dir(parent);
            }
            bump();
        }
    }
}

/// One save over a project, one removal, or one placing of a new project at a time, in this process.
fn changing() -> MutexGuard<'static, ()> {
    static CHANGING: Mutex<()> = Mutex::new(());
    CHANGING.lock().unwrap_or_else(|e| e.into_inner())
}

/// The projects open in an editor, by folder; true for one that is removed when it is closed.
fn registry() -> MutexGuard<'static, HashMap<PathBuf, bool>> {
    static OPEN: std::sync::LazyLock<Mutex<HashMap<PathBuf, bool>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    OPEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// One name for a folder however its path is spelled.
fn registry_name(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn is_open(dir: &Path) -> bool {
    registry().contains_key(&registry_name(dir))
}

/// Mark an open project to be removed when it is closed; false when it is not open.
fn remove_when_closed(dir: &Path) -> bool {
    registry().get_mut(&registry_name(dir)).map(|remove| *remove = true).is_some()
}

/// Save over a project that an editor has open. The frames file is that editor's own and is not
/// written; the new pictures go beside the old ones, the project file is replaced in one rename,
/// and then what it no longer names is removed. A failure leaves the project as it was.
fn save_in_place(dir: &Path, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<Vec<String>, String> {
    let kept = Recording::open_saved(dir).map_err(|_| "the saved recording's frames can no longer be read.".to_string())?;
    if !same_recording(&kept, snap.rec) {
        return Err("this tour's saved recording is open in an editor and belongs to another recording.".into());
    }
    let frames_bytes = std::fs::metadata(dir.join(FRAMES_FILE)).map_err(|e| sentence("the saved recording's frames can no longer be read", e))?.len();
    let (file, blobs, left_out) = state::to_file(key, snap, now_ms(), frames_bytes);
    let text = project_text(&file)?;
    let before = read_project(dir).ok().map(|old| named_files(&old));
    let written = blobs.iter().try_for_each(|blob| write_blob(dir, blob)).and_then(|()| {
        let path = dir.join(PROJECT_FILE);
        let tmp = temp_beside(&path);
        write_whole(&tmp, text.as_bytes()).and_then(|()| (steps.rename)(&tmp, &path)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            sentence("the project file could not be written", e)
        })
    });
    match written {
        Ok(()) => {
            tidy(dir, &blobs.iter().map(|b| b.name.clone()).collect());
            Ok(left_out)
        }
        Err(e) => {
            // Take away the pictures this save added; without the old list nothing is removed.
            if let Some(before) = before {
                tidy(dir, &before);
            }
            Err(e)
        }
    }
}

/// Put everything a project holds into the new folder `staging`, the project file last.
fn fill(staging: &Path, key: &ProjectKey, snap: &Snapshot<'_>, steps: &Steps<'_>) -> Result<Vec<String>, String> {
    let frames_bytes = (steps.keep_frames)(snap.rec, staging).map_err(|e| one_sentence(&e))?;
    let (file, blobs, left_out) = state::to_file(key, snap, now_ms(), frames_bytes);
    let text = project_text(&file)?;
    blobs.iter().try_for_each(|blob| write_blob(staging, blob))?;
    write_whole(&staging.join(PROJECT_FILE), text.as_bytes()).map_err(|e| sentence("the project file could not be written", e))?;
    Ok(left_out)
}

fn project_text(file: &ProjectFile) -> Result<String, String> {
    let text = state::to_text(file).map_err(|e| sentence("the project could not be written down", e))?;
    if text.len() > MAX_PROJECT_JSON_BYTES {
        return Err("the project is too large to be saved.".into());
    }
    Ok(text)
}

/// Whether two recordings are the same one, as far as their indexes tell.
fn same_recording(a: &Recording, b: &Recording) -> bool {
    (a.width, a.height, a.end_ms, a.frames.len()) == (b.width, b.height, b.end_ms, b.frames.len()) && a.frames.iter().zip(&b.frames).all(|(x, y)| (x.t_ms, x.cursor) == (y.t_ms, y.cursor))
}

/// The pictures and the logo a project file names, as paths inside the project with `/`.
fn named_files(file: &ProjectFile) -> BTreeSet<String> {
    let pictures = file.items.iter().filter_map(|entry| match entry {
        ItemEntry::Item(item) => match &item.shape {
            SavedShape::Image { picture, .. } => Some(picture.clone()),
            _ => None,
        },
        ItemEntry::Unreadable(_) => None,
    });
    pictures.chain(file.edit.tour_doc.logo.clone()).collect()
}

/// Write one picture or logo: under a temporary name, on the disk, then renamed. A file of that
/// name is trusted only when it has the right size; one cut short by an earlier save is replaced.
fn write_blob(dir: &Path, blob: &Blob) -> Result<(), String> {
    let failed = |e: io::Error| sentence("a picture of the project could not be written", e);
    let path = match blob.name.strip_prefix("pictures/") {
        Some(file) if state::is_picture_name(&blob.name) => dir.join(PICTURES).join(file),
        None if state::is_logo_name(&blob.name) => dir.join(&blob.name),
        _ => return Err("a picture of the project has a name that cannot be used.".into()),
    };
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() == blob.bytes.len() as u64) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    let tmp = temp_beside(&path);
    write_whole(&tmp, &blob.bytes).and_then(|()| patiently(|| std::fs::rename(&tmp, &path))).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        failed(e)
    })
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
    known(name) || temp_of(name).is_some_and(known)
}

/// A file in a project's `pictures` folder that only the store puts there.
fn is_picture_file(name: &str) -> bool {
    let known = |n: &str| state::is_picture_name(&format!("pictures/{n}"));
    known(name) || temp_of(name).is_some_and(known)
}

/// Remove from a project folder the pictures and logos that are not in `keep`, and temporary
/// files a cut-off save left. Nothing else is touched, and a file that cannot be removed stays.
fn tidy(dir: &Path, keep: &BTreeSet<String>) {
    for (name, path) in files_in(dir) {
        let stale = temp_of(&name).is_some() && is_top_file(&name);
        if stale || (state::is_logo_name(&name) && !keep.contains(&name)) {
            let _ = std::fs::remove_file(path);
        }
    }
    let pictures = dir.join(PICTURES);
    for (name, path) in files_in(&pictures) {
        if is_picture_file(&name) && !keep.contains(&format!("pictures/{name}")) {
            let _ = std::fs::remove_file(path);
        }
    }
    let _ = std::fs::remove_dir(&pictures);
}

/// Remove a project: its project file first, so that what is left is no project, then every
/// other file the store wrote, then the folders if they are empty. A file somebody else put
/// there stays, with its folder, and is not an error.
fn remove_files(dir: &Path) -> io::Result<()> {
    match patiently(|| std::fs::remove_file(dir.join(PROJECT_FILE))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let pictures = dir.join(PICTURES);
    let ours = files_in(dir).into_iter().filter(|(name, _)| is_top_file(name)).chain(files_in(&pictures).into_iter().filter(|(name, _)| is_picture_file(name)));
    let mut failed = None;
    for (_, path) in ours {
        match patiently(|| std::fs::remove_file(&path)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => failed = failed.or(Some(e)),
            _ => {}
        }
    }
    let _ = std::fs::remove_dir(&pictures);
    let _ = std::fs::remove_dir(dir);
    failed.map_or(Ok(()), Err)
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

/// The size of a project's files together.
fn bytes_in(dir: &Path) -> u64 {
    files_in(dir).into_iter().chain(files_in(&dir.join(PICTURES))).filter_map(|(_, path)| std::fs::symlink_metadata(path).ok()).map(|m| m.len()).sum()
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
/// the folder holds no project file. The cheap check: the project file parses, its address, tour
/// and chapter are the folder's, and the frames file is there at the size it was saved with.
fn read_info(dir: &Path, site: &str, tour: &str, chapter: &str, server: Option<&str>) -> Option<ProjectInfo> {
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

/// Sixteen hex characters no other call gets: the name of a staging folder or a temporary file.
fn random_name() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    hex16(format!("{}-{nanos}-{}", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed)).as_bytes())
}

fn is_random_name(s: &str) -> bool {
    s.len() == RANDOM_CHARS && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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
fn sentence(what: &str, why: impl std::fmt::Display) -> String {
    one_sentence(&format!("{what}: {why}"))
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
        let (mut bytes, mut rows) = (Vec::new(), Vec::new());
        for i in 0..n {
            let frame = vec![i as u8 + 1; 20 + i];
            rows.push(serde_json::json!([bytes.len(), frame.len(), i == 0, i * 100, null, null]));
            bytes.extend(frame);
        }
        (bytes, serde_json::json!({"format": 1, "width": 100, "height": 80, "end_ms": n * 100, "frames": rows}).to_string())
    }

    /// A recording of `n` frames of 100 x 80 in the folder `<root>/<name>`, as an editor has one.
    fn recorded(root: &Path, name: &str, n: usize) -> Recording {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let (bytes, index) = frames_and_index(n);
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
            let store = Store::at(tmp.path().join("projects"));
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

    fn real_rename(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn real_frames(rec: &Recording, dir: &Path) -> Result<u64, String> {
        rec.save_into(dir)
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
    fn the_default_folder_is_called_projects() {
        assert!(Store::default_root().ends_with("projects"));
    }

    #[test]
    fn a_saved_project_loads_back_exactly_as_it_was_saved() {
        let w = World::new();
        let before = epoch();
        let saved = w.save(&key_1());
        assert!(epoch() > before, "a save tells the pages to read the list again");
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
        let full_disk = |_: &Recording, dir: &Path| -> Result<u64, String> {
            std::fs::write(dir.join("frames.zst.1-2.tmp"), [1u8; 40]).unwrap();
            Err("could not keep the frames: There is not enough space on the disk. (os error 112)".to_string())
        };
        let err = w.store.save_with(&key_1(), &w.snap(), &Steps { keep_frames: &full_disk, rename: &real_rename }).unwrap_err();
        assert!(err.contains("not enough space"), "{err}");
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
        assert!(err.contains("the rename was refused"), "{err}");
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
        let before = epoch();
        let snap = Snapshot { rec: &l.rec, title: "Chapter one, again", tour_title: "Customer onboarding", steps: 2, edit: &edit, events: &l.restored.events, items: &items };
        let second = w.store.save(&key_1(), &snap).unwrap();
        assert!(epoch() > before);
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
        assert!(err.contains("the rename was refused") && !err.contains(&*w.tmp.path().to_string_lossy()), "{err}");
        assert_eq!(contents(&dir), whole, "every file is as the first save left it, and no new one is beside them");
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
        assert_eq!(tree(w.store.root()).iter().filter(|n| n.contains(STAGING)).count(), 0);
        assert_eq!(loaded(&w.store, &key_1()).restored.title, "Chapter one");

        let saved = w.store.save(&key_1(), &snap).unwrap();
        assert_eq!((saved.info.title.as_str(), saved.info.problem), ("Shorter", None));
        assert_eq!(tree(&dir), vec![FRAMES_FILE.to_string(), "notes.txt".to_string(), PROJECT_FILE.to_string(), INDEX_FILE.to_string()], "the old project's files are gone, and a file that was nobody's is kept");
        assert_eq!(std::fs::read(dir.join(FRAMES_FILE)).unwrap(), frames_and_index(4).0);
        assert_eq!(tree(w.store.root()).iter().filter(|n| n.contains(STAGING)).count(), 0);
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
            rec.save_into(to)
        };
        let err = w.store.save_with(&key_1(), &snap, &Steps { keep_frames: &slow, rename: &real_rename }).unwrap_err();
        assert!(err.contains("open in an editor"), "{err}");
        assert_eq!(contents(&dir), whole);
        assert_eq!(tree(w.store.root()).iter().filter(|n| n.contains(STAGING)).count(), 0);
        assert_eq!(opened.borrow().as_ref().unwrap().restored.title, "Chapter one");
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
        let newer = serde_json::json!({"format": 2, "app": "9.0.0", "saved_at": 1_800_000_000_000i64, "server": SITE_A, "tour_id": TOUR_1, "chapter_id": CH_1, "tour_title": "Customer onboarding", "title": "From later", "edit": "something new"}).to_string();
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
        let before = epoch();
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(1), "one project was there to remove");
        assert!(epoch() > before);
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
        assert_eq!(w.store.remove_tour(SITE_A, TOUR_1), Ok(2));
        // The open one is whole until its editor closes; the other is gone at once.
        assert_eq!(keys(&w.store.list()), vec![key(SITE_A, TOUR_1, CH_1)]);
        assert_eq!(std::fs::read(open.info.dir.join(FRAMES_FILE)).unwrap(), frames_and_index(FRAMES).0);
        assert_eq!(load_err(&w.store, &key(SITE_A, TOUR_1, CH_1)), LoadError::AlreadyOpen);
        let before = epoch();
        drop(open);
        assert!(epoch() > before);
        assert_eq!(w.store.list(), vec![]);
        assert_eq!(tree(w.store.root()), Vec::<String>::new());
        assert_eq!(load_err(&w.store, &key(SITE_A, TOUR_1, CH_1)), LoadError::NotSaved);
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
        std::fs::create_dir(w.store.root().join(".staging-0123456789abcdef")).unwrap();
        std::fs::write(w.store.root().join(".staging-0123456789abcdef").join(PROJECT_FILE), b"{}").unwrap();
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

    #[test]
    fn only_old_staging_folders_with_the_exact_name_are_swept() {
        let w = World::new();
        w.save(&key_1());
        let root = w.store.root().to_path_buf();
        let cut_off = root.join(".staging-0123456789abcdef");
        std::fs::create_dir_all(cut_off.join(PICTURES)).unwrap();
        std::fs::write(cut_off.join(FRAMES_FILE), b"half").unwrap();
        let not_ours = [".staging-", ".staging-0123456789abcde", ".staging-0123456789abcdef0", ".staging-0A1B2C3D4E5F6A7B", ".staging-0123456789abcdeg", "staging-0123456789abcdef", ".staging-0123456789abcdef.bak"];
        for name in not_ours {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("kept.txt"), b"x").unwrap();
        }
        // A file with the exact name, and a folder with it one level down.
        std::fs::write(root.join(".staging-fedcba9876543210"), b"a file").unwrap();
        let deeper = root.join(site_of(SITE_A)).join(".staging-0123456789abcdef");
        std::fs::create_dir(&deeper).unwrap();
        let everything = tree(&root);

        // Not old enough yet: a save may still be writing it.
        w.store.sweep_staging(Duration::from_secs(86_400));
        assert_eq!(tree(&root), everything);
        // A day and an hour later.
        let before = epoch();
        w.store.sweep_staging_at(Duration::from_secs(86_400), SystemTime::now() + Duration::from_secs(90_000));
        let left = tree(&root);
        assert!(!cut_off.exists());
        assert_eq!(left, everything.into_iter().filter(|n| !n.starts_with(".staging-0123456789abcdef/")).collect::<Vec<_>>(), "only the cut-off save is gone");
        assert_eq!(epoch(), before.max(epoch()), "sweeping is not a change to the list");
        assert_eq!(keys(&w.store.list()), vec![key_1()]);
        loaded(&w.store, &key_1());
        // A projects folder that does not exist is nothing to sweep.
        Store::at(w.tmp.path().join("never made")).sweep_staging(Duration::ZERO);
        assert!(!w.tmp.path().join("never made").exists());
    }
}
