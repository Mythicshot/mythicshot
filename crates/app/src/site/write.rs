//! Adding a tour to a site folder, or replacing one, so that the site works at every moment.
//!
//! One add writes to a site at a time: an in-process lock keyed by the site's path and a lock file in
//! the site folder, so two windows or two copies of the app cannot lose each other's tours. Under the
//! lock an add first finishes what an interrupted one left (a replace stopped between its two renames is
//! put back) and clears its leftovers, then reads `tours.json`. The new tour is written into a temporary
//! folder beside the others and swapped into place only when it is complete; `tours.json` and the index
//! follow, each through a temporary file and a rename. Any failure, cancel or panic before the end puts
//! back what was there and removes what this call made. Nothing this feature did not create is ever
//! deleted or overwritten, and no link is followed.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use image::ImageEncoder as _;

use super::{Assets, FAVICON, Folder, LOCK, SiteList, THUMB, TourEntry, address, assets, check_slug, index_page, inspect, site_title};
use crate::tour::TourSummary;

/// An empty marker some hosts read as "serve these files as they are".
const NOJEKYLL: &str = ".nojekyll";
/// What a second add to the same site is told.
pub const BUSY: &str = "Another export is adding to this site. Try again when it finishes.";

/// What [`add_tour`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct SiteSummary {
    /// The site folder.
    pub dir: PathBuf,
    pub slug: String,
    /// A tour at this address was replaced.
    pub replaced: bool,
    /// Tours in the site now.
    pub tours: usize,
    /// The tour as written, with its final folder as `path`.
    pub tour: TourSummary,
    /// Something worth knowing that did not stop the add: an interrupted replace that was put back, or
    /// an old copy that could not be removed.
    pub note: Option<String>,
}

/// How long a rename or removal that Windows refuses because a file is in use is tried again.
#[cfg(not(test))]
const RETRY_FOR: Duration = Duration::from_secs(2);
#[cfg(test)]
const RETRY_FOR: Duration = Duration::from_millis(300);
/// A lock file older than this is taken to be left by a crash, whoever wrote it.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(2 * 60 * 60);
/// A lock file that cannot be read is given this long to be filled in by the add that just made it.
const LOCK_UNREADABLE_GRACE: Duration = Duration::from_secs(60);
/// A staged temporary file older than this was left by a crash.
const TEMP_FILE_STALE_AFTER: Duration = Duration::from_secs(5 * 60);

/// Windows refuses with these while a file is open in another program or being scanned: access denied
/// (a pending delete or a scanner), sharing violation and lock violation.
fn busy(e: &std::io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// Run `f`, trying again for up to [`RETRY_FOR`] while the error says a file is busy.
fn retry<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let start = Instant::now();
    let mut wait = Duration::from_millis(20);
    loop {
        match f() {
            Err(e) if busy(&e) && start.elapsed() + wait <= RETRY_FOR => {
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_millis(250));
            }
            other => return other,
        }
    }
}

// ---- Adds in flight, and the lock ----

/// Adds running in this process, so quitting can wait for one to finish or roll back instead of
/// stopping it between two renames.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// The sites this process is adding to, by [`site_key`].
static HELD: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Adds to a site running now in this process.
pub fn adds_in_flight() -> usize {
    IN_FLIGHT.load(Ordering::SeqCst)
}

/// On quit, after cancelling: wait up to `limit` for every add to finish or roll back. A cancelled add
/// stops before its next picture; one already swapping finishes within a few seconds.
pub fn wait_for_adds(limit: Duration) {
    let until = Instant::now() + limit;
    while adds_in_flight() > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct InFlight;

impl InFlight {
    fn new() -> InFlight {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        InFlight
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The same text for every spelling of one folder: its canonical path (that of its parent when it does
/// not exist yet), without letter case on Windows, where names do not have any.
fn site_key(dir: &Path) -> String {
    let full = canonical(dir).unwrap_or_else(|| dir.to_path_buf());
    let text = full.to_string_lossy().into_owned();
    if cfg!(windows) { text.to_lowercase() } else { text }
}

/// The canonical path of `dir`, or of its parent joined with its name when `dir` does not exist yet.
fn canonical(dir: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(dir).ok().or_else(|| {
        let parent = dir.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        Some(std::fs::canonicalize(parent).ok()?.join(dir.file_name()?))
    })
}

/// Whether a process with this id is running. Unknown counts as running; age still frees its lock.
fn process_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        // SAFETY: plain calls with a process id; the handle is closed before returning.
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => {
                    let mut code = 0u32;
                    let ok = GetExitCodeProcess(h, &mut code).is_ok();
                    let _ = CloseHandle(h);
                    !ok || code == STILL_ACTIVE.0 as u32
                }
                // No such process (invalid parameter); anything else, such as access denied, means it exists.
                Err(e) => e.code() != windows::core::HRESULT::from_win32(87),
            }
        }
    }
    #[cfg(not(windows))]
    {
        let proc = Path::new("/proc");
        !proc.is_dir() || proc.join(pid.to_string()).exists()
    }
}

/// What a lock file says: the owner's process id and when it was taken, in seconds since 1970.
fn read_lock(text: &str) -> Option<(u32, u64)> {
    let mut pid = None;
    let mut time = None;
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("pid", v)) => pid = v.trim().parse().ok(),
            Some(("time", v)) => time = v.trim().parse().ok(),
            _ => {}
        }
    }
    Some((pid?, time?))
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether a lock file found in a site was left by an add that is no longer running. This process
/// already holds the site in memory, so a lock naming this process is a leftover too.
fn lock_is_stale(text: &str, modified: Option<SystemTime>) -> bool {
    let age_of = |t: SystemTime| SystemTime::now().duration_since(t).unwrap_or_default();
    match read_lock(text) {
        Some((pid, time)) => pid == std::process::id() || !process_alive(pid) || now_secs().saturating_sub(time) > LOCK_STALE_AFTER.as_secs(),
        None => modified.is_some_and(|m| age_of(m) > LOCK_UNREADABLE_GRACE),
    }
}

enum Step {
    File(PathBuf),
    /// A folder this call created, removed only when empty.
    Dir(PathBuf),
    /// A folder under a unique temporary name this call made, removed with everything in it.
    Tree(PathBuf),
}

/// One add's hold on a site: the in-process lock, the lock file, and what to undo if the add does not
/// finish. Dropping it, on success, failure or panic, removes the lock file, undoes what is left to
/// undo, and releases the site.
struct Session {
    key: String,
    lock: Option<PathBuf>,
    steps: Vec<Step>,
}

impl Session {
    fn begin(dir: &Path) -> Result<Session, String> {
        let key = site_key(dir);
        let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
        if !held.get_or_insert_with(HashSet::new).insert(key.clone()) {
            return Err(BUSY.to_string());
        }
        Ok(Session { key, lock: None, steps: Vec::new() })
    }

    /// Create the lock file in the site folder, exclusively. One left by an add that is gone is replaced.
    fn take_lock(&mut self, dir: &Path) -> Result<(), String> {
        let path = dir.join(LOCK);
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    self.lock = Some(path.clone());
                    f.write_all(format!("pid {}\ntime {}\n", std::process::id(), now_secs()).as_bytes()).map_err(|e| format!("Could not write {LOCK}: {e}"))?;
                    return Ok(());
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let meta = path.symlink_metadata().map_err(|e| format!("Could not check {LOCK}: {e}"))?;
                    if !meta.is_file() {
                        return Err(format!("{LOCK} in this folder is not a plain file, so Mythicshot will not use this folder."));
                    }
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    if !lock_is_stale(&text, meta.modified().ok()) {
                        return Err(BUSY.to_string());
                    }
                    match retry(|| std::fs::remove_file(&path)) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(format!("Could not remove the lock {LOCK} an earlier add left: {e}")),
                    }
                }
                Err(e) => return Err(format!("Could not write {LOCK}: {e}")),
            }
        }
        Err(BUSY.to_string())
    }

    /// Create the folder `dir` unless it is there. Only its last part is created.
    fn ensure_dir(&mut self, dir: &Path) -> Result<(), String> {
        match std::fs::create_dir(dir) {
            Ok(()) => {
                self.steps.push(Step::Dir(dir.to_path_buf()));
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.symlink_metadata().is_ok_and(|m| m.is_dir()) => Ok(()),
            Err(e) => Err(format!("Could not create the folder {}: {e}", shown(dir))),
        }
    }

    /// Add `name` with `bytes` in `dir` only if nothing has that name; never replaces anything.
    fn add_if_missing(&mut self, dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
        let target = dir.join(name);
        match target.symlink_metadata() {
            Ok(m) if m.is_file() => return Ok(()),
            Ok(_) => return Err(format!("{name} in this site is not a plain file, so Mythicshot will not use it.")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Could not check {name}: {e}")),
        }
        let tmp = stage(dir, bytes, name)?;
        match tmp.persist_noclobber(&target) {
            Ok(_) => {
                self.steps.push(Step::File(target));
                Ok(())
            }
            // Something of that name appeared in the meantime: it stays as it is.
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(format!("Could not write {name}: {}", e.error)),
        }
    }

    /// The add is complete: nothing is undone.
    fn finish(&mut self) {
        self.steps.clear();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take()
            && let Err(e) = retry(|| std::fs::remove_file(&lock))
        {
            eprintln!("Add to site: could not remove {LOCK}: {e}");
        }
        for step in self.steps.drain(..).rev() {
            let r = match &step {
                Step::File(p) => retry(|| std::fs::remove_file(p)),
                Step::Dir(p) => std::fs::remove_dir(p),
                Step::Tree(p) => remove_ours(p),
            };
            if let Err(e) = r
                && e.kind() != std::io::ErrorKind::NotFound
            {
                let (Step::File(p) | Step::Dir(p) | Step::Tree(p)) = &step;
                eprintln!("Add to site: could not remove {}: {e}", shown(p));
            }
        }
        let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = held.as_mut() {
            h.remove(&self.key);
        }
    }
}

// ---- Names ----

/// Eight lowercase hex digits, different every call.
fn hex8() -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    h.write_u128(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    format!("{:08x}", h.finish() as u32)
}

/// A temporary folder name inside `tours` for this slug: `tmp-<slug>-<8 hex>` or `old-<slug>-<8 hex>`.
/// No tour can have such a name, since slugs never start with `tmp-` or `old-`.
fn temp_name(kind: &str, slug: &str, tours: &Path) -> PathBuf {
    loop {
        let p = tours.join(format!("{kind}-{slug}-{}", hex8()));
        if p.symlink_metadata().is_err() {
            return p;
        }
    }
}

/// For a temporary folder name this feature gives, its kind (`tmp` or `old`) and the slug it belongs to.
fn temp_owner(name: &str) -> Option<(&str, &str)> {
    let (kind, rest) = name.split_once('-')?;
    if kind != "tmp" && kind != "old" {
        return None;
    }
    let (slug, hex) = rest.rsplit_once('-')?;
    let hex_ok = hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (hex_ok && check_slug(slug).is_ok()).then_some((kind, slug))
}

/// Whether `name` is exactly a temporary folder name this feature gives `slug`.
#[cfg(test)]
fn leftover_of(name: &str, slug: &str) -> bool {
    temp_owner(name).is_some_and(|(_, s)| s == slug)
}

/// Whether `name` is a temporary file this feature stages beside the site's files: `.mythicshot-`,
/// six letters or digits, `.tmp`.
fn staged_name(name: &str) -> bool {
    name.strip_prefix(".mythicshot-").and_then(|r| r.strip_suffix(".tmp")).is_some_and(|m| m.len() == 6 && m.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// The generator recorded in `tours.json`.
fn generator() -> String {
    format!("mythicshot {}", env!("CARGO_PKG_VERSION"))
}

/// The last part of a path, for messages that do not show where the site is.
fn shown(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ---- Files ----

/// `bytes` in a new temporary file in `dir`, written and flushed to disk, readable by everyone like any web file.
fn stage(dir: &Path, bytes: &[u8], what: &str) -> Result<tempfile::NamedTempFile, String> {
    let mut tmp = tempfile::Builder::new().prefix(".mythicshot-").suffix(".tmp").tempfile_in(dir).map_err(|e| format!("Could not write {what}: {e}"))?;
    tmp.write_all(bytes).and_then(|_| tmp.as_file().sync_all()).map_err(|e| format!("Could not write {what}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644)).map_err(|e| format!("Could not write {what}: {e}"))?;
    }
    Ok(tmp)
}

/// Rename a staged file over `target`, trying again while it is busy. The staged file is removed on failure.
fn commit(tmp: tempfile::NamedTempFile, target: &Path, what: &str) -> Result<(), String> {
    let mut tmp = Some(tmp);
    retry(|| match tmp.take().expect("staged file").persist(target) {
        Ok(_) => Ok(()),
        Err(e) => {
            tmp = Some(e.file);
            Err(e.error)
        }
    })
    .map_err(|e| format!("Could not write {what}: {e}"))
}

/// Remove a folder this feature made (by its temporary name), trying again while a file in it is busy.
fn remove_ours(dir: &Path) -> std::io::Result<()> {
    match dir.symlink_metadata() {
        Ok(m) if m.is_dir() => retry(|| std::fs::remove_dir_all(dir)),
        Ok(_) => retry(|| std::fs::remove_file(dir)),
        Err(e) => Err(e),
    }
}

/// Remove staged files a crash left in `dir`: exactly this feature's name, plain files, older than
/// [`TEMP_FILE_STALE_AFTER`]. A failure is only reported.
fn clean_stale_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(meta) = e.path().symlink_metadata() else { continue };
        let old = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|a| a > TEMP_FILE_STALE_AFTER);
        if staged_name(&name) && meta.is_file() && old
            && let Err(err) = retry(|| std::fs::remove_file(e.path()))
        {
            eprintln!("Add to site: could not remove the leftover file {name}: {err}");
        }
    }
}

/// Remove every temporary folder an earlier add left in `tours`, whatever its slug: only real folders
/// whose name is exactly one this feature gives and that no tour in the list has. A failure is only reported.
fn sweep(tours: &Path, list: &SiteList) {
    let Ok(entries) = std::fs::read_dir(tours) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if temp_owner(&name).is_none() || list.tours.iter().any(|t| t.slug == name) {
            continue;
        }
        if e.path().symlink_metadata().is_ok_and(|m| m.is_dir())
            && let Err(err) = retry(|| std::fs::remove_dir_all(e.path()))
        {
            eprintln!("Add to site: could not remove the leftover folder tours/{name}: {err}");
        }
    }
}

/// Finish what an interrupted replace left: a listed tour whose folder is missing while exactly one
/// earlier copy of it (`old-<slug>-<8 hex>`) is there gets that copy back, which matches `tours.json`.
/// With more than one copy it cannot tell which to keep, and says so.
fn recover(tours: &Path, list: &SiteList) -> Result<Vec<String>, String> {
    let Ok(entries) = std::fs::read_dir(tours) else { return Ok(Vec::new()) };
    let names: Vec<String> = entries.flatten().filter(|e| e.path().symlink_metadata().is_ok_and(|m| m.is_dir())).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let mut notes = Vec::new();
    for t in &list.tours {
        let target = tours.join(&t.slug);
        if target.symlink_metadata().is_ok() {
            continue;
        }
        let mut olds: Vec<&String> = names.iter().filter(|n| temp_owner(n) == Some(("old", t.slug.as_str()))).collect();
        olds.sort();
        match olds.as_slice() {
            [] => {}
            [old] => {
                retry(|| std::fs::rename(tours.join(old), &target)).map_err(|e| format!("{} is missing after an interrupted add, and its earlier copy tours/{old}/ could not be put back ({e}). Rename tours/{old}/ to {} and try again.", address(&t.slug), address(&t.slug)))?;
                notes.push(format!("An earlier add of {} was interrupted; the previous version was put back.", address(&t.slug)));
            }
            many => {
                let found: Vec<String> = many.iter().map(|n| format!("tours/{n}/")).collect();
                return Err(format!("{} is missing after an interrupted add, and more than one earlier copy of it is in the site: {}. Rename the one to keep to {}, delete the others, and add the tour again.", address(&t.slug), found.join(", "), address(&t.slug)));
            }
        }
    }
    Ok(notes)
}

/// Every listed tour has its plain folder, and nothing in `tours` this feature did not make is at `slug`
/// under any spelling (Windows does not tell `Mine` from `mine`).
fn check_tours(tours: &Path, list: &SiteList, slug: &str) -> Result<(), String> {
    for t in &list.tours {
        match tours.join(&t.slug).symlink_metadata() {
            Ok(m) if m.file_type().is_symlink() => return Err(format!("tours.json lists {}, which is a link, so Mythicshot will not write into this site.", address(&t.slug))),
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(format!("tours.json lists {}, which is not a folder. Remove its entry from tours.json or put the tour back.", address(&t.slug))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(format!("tours.json lists {}, but that folder is missing. Put the tour back or remove its entry from tours.json.", address(&t.slug))),
            Err(e) => return Err(format!("Could not check {}: {e}", address(&t.slug))),
        }
    }
    let entries = match std::fs::read_dir(tours) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("Could not read the tours folder: {e}")),
    };
    let listed = list.tours.iter().any(|t| t.slug == slug);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.eq_ignore_ascii_case(slug) && !(listed && name == slug) {
            return Err(format!("{} is already in this site and was not made by Mythicshot, so it will not be replaced. Choose another address.", address(slug)));
        }
    }
    Ok(())
}

/// Whether a tour folder may hold a file of this name: what the tour export writes.
fn tour_file(name: &str) -> bool {
    let picture = |stem: &str| ["png", "jpg"].iter().any(|ext| name.strip_prefix(stem).and_then(|r| r.strip_suffix(ext)).is_some());
    let step = name.strip_prefix("step-").and_then(|r| r.split_once('.')).is_some_and(|(n, ext)| n.len() >= 3 && n.bytes().all(|b| b.is_ascii_digit()) && (ext == "png" || ext == "jpg"));
    matches!(name, "index.html" | "tour.json" | THUMB) || step || picture("logo.")
}

/// Files and folders in a tour's folder the tour export does not write, so replacing it would lose them.
fn foreign_files(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("Could not read {}: {e}", shown(dir)))?;
    let mut out: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let file = e.path().symlink_metadata().is_ok_and(|m| m.is_file());
        if !file {
            out.push(format!("{name}/"));
        } else if !tour_file(&name) {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// Refuse a whole drive and the main folders of the person's account (home, Desktop, Documents,
/// Downloads) as a site folder: a site belongs in a folder of its own, which may be inside one of them.
pub fn check_dedicated(dir: &Path) -> Result<(), String> {
    let Some(target) = canonical(dir) else { return Ok(()) };
    let ask = |what: &str| Err(format!("{what} is not a good place for a site. Choose or make a folder just for the site, such as one named site inside it."));
    if target.parent().is_none() {
        return ask(&format!("The whole drive {}", dir.display()));
    }
    let same = |a: &Path, b: &Path| if cfg!(windows) { a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase() } else { a == b };
    if let Some(user) = directories::UserDirs::new() {
        let main = [Some(user.home_dir()), user.desktop_dir(), user.document_dir(), user.download_dir()];
        for d in main.into_iter().flatten() {
            if std::fs::canonicalize(d).is_ok_and(|c| same(&c, &target)) {
                return ask(&shown(d));
            }
        }
    }
    Ok(())
}

/// A neutral 32 by 32 icon: a slate rounded square with a light dot. Made here, so it is no one's logo.
pub fn favicon() -> Vec<u8> {
    const S: u32 = 32;
    let img = image::RgbaImage::from_fn(S, S, |x, y| {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        // Rounded square, corner radius 7.
        let r = 7.0f32;
        let (cx, cy) = (fx.clamp(r, S as f32 - r), fy.clamp(r, S as f32 - r));
        let corner = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
        let square = (r + 0.5 - corner).clamp(0.0, 1.0);
        let dot = (6.5 - ((fx - 16.0).powi(2) + (fy - 16.0).powi(2)).sqrt()).clamp(0.0, 1.0);
        let mix = |a: f32, b: f32| (a + (b - a) * dot).round() as u8;
        image::Rgba([mix(70.0, 226.0), mix(82.0, 232.0), mix(102.0, 240.0), (square * 255.0).round() as u8])
    });
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(img.as_raw(), S, S, image::ExtendedColorType::Rgba8).expect("a PNG in memory");
    // An ICO file holding one PNG image: the header, one directory entry, the picture.
    let mut ico = vec![0, 0, 1, 0, 1, 0, S as u8, S as u8, 0, 0, 1, 0, 32, 0];
    ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(&png);
    ico
}

/// Put a site's tours back as they were before a swap that could not be finished. `new_at_target` says
/// whether the new tour already sits at `tours/<slug>`. Ok when the previous state is back; otherwise
/// which folder holds which version, since a complete copy is never deleted when the other could not be
/// put back.
fn unswap(tours: &Path, slug: &str, new_at_target: bool, old: Option<&Path>) -> Result<(), String> {
    let target = tours.join(slug);
    let aside = temp_name("tmp", slug, tours);
    if new_at_target && let Err(e) = retry(|| std::fs::rename(&target, &aside)) {
        return Err(match old {
            Some(o) => format!("The new version could not be moved back out ({e}): it is in {}, and the previous version is kept in tours/{}/.", address(slug), shown(o)),
            None => format!("The new version could not be moved back out ({e}): it is in {}, but the site's list does not name it yet.", address(slug)),
        });
    }
    if let Some(o) = old
        && let Err(e) = retry(|| std::fs::rename(o, &target))
    {
        let new_copy = if new_at_target { format!(" and the new one in tours/{}/", shown(&aside)) } else { String::new() };
        return Err(format!(
            "The previous version could not be put back ({e}), so {} is missing: the previous version is in tours/{}/{new_copy}. The next add to this site puts the previous version back by itself, or rename tours/{}/ to {}.",
            address(slug),
            shown(o),
            shown(o),
            address(slug)
        ));
    }
    if new_at_target && let Err(e) = remove_ours(&aside) {
        eprintln!("Add to site: could not remove tours/{}: {e}", shown(&aside));
    }
    Ok(())
}

/// The message for a failure after the swap began: the cause, then either "unchanged" or where the tour is.
fn after_unswap(cause: String, restored: Result<(), String>) -> String {
    match restored {
        Ok(()) => format!("{cause}. The site is unchanged."),
        Err(state) => format!("{cause}. {state}"),
    }
}

/// Add the tour to the site in `site_dir`, or replace the one at `slug`.
/// `write(new_dir, assets)` must create `new_dir` and write the tour into it (it is `write_tour` with `Page::Site`).
/// `now` is the UTC time for the entry. Nothing outside `site_dir` is touched.
///
/// The order: refuse a drive or a main folder; take the site (in this process, then the lock file);
/// finish an interrupted replace and clear leftovers; read and check `tours.json`; create what is missing
/// of the site (the folder itself, `tours`, `assets`, the shared player files, `.nojekyll`, the icon
/// unless the folder has its own), never replacing anything; write the tour into
/// `tours/tmp-<slug>-<8 hex>`; move the old tour aside to `tours/old-<slug>-<8 hex>` and the new one into
/// place; write `tours.json`, then `index.html`; remove the old tour and the lock. A failure before the
/// end leaves the site as it was, byte for byte, and removes what this call made.
pub fn add_tour(site_dir: &Path, title_of_site: &str, slug: &str, tour_title: &str, now: &str, write: impl FnOnce(&Path, &Assets) -> Result<TourSummary, String>) -> Result<SiteSummary, String> {
    check_slug(slug)?;
    check_dedicated(site_dir)?;
    let _in_flight = InFlight::new();
    let mut session = Session::begin(site_dir)?;
    // A folder that cannot be used is refused before anything is made in it.
    inspect(site_dir)?;
    session.ensure_dir(site_dir)?;
    session.take_lock(site_dir)?;
    let result = add_locked(&mut session, site_dir, title_of_site, slug, tour_title, now, write);
    if result.is_ok() {
        session.finish();
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn add_locked(
    session: &mut Session,
    site_dir: &Path,
    title_of_site: &str,
    slug: &str,
    tour_title: &str,
    now: &str,
    write: impl FnOnce(&Path, &Assets) -> Result<TourSummary, String>,
) -> Result<SiteSummary, String> {
    let tours = site_dir.join("tours");
    let assets_dir = site_dir.join("assets");
    let list_path = site_dir.join("tours.json");
    let index_path = site_dir.join("index.html");
    // Under the lock: what a crash left, then the list as it is now.
    clean_stale_files(site_dir);
    clean_stale_files(&assets_dir);
    let mut notes = Vec::new();
    let list = match inspect(site_dir)? {
        Folder::Site(list) => {
            notes = recover(&tours, &list)?;
            sweep(&tours, &list);
            check_tours(&tours, &list, slug)?;
            list
        }
        Folder::New => SiteList { site: super::SITE_VERSION, generator: generator(), title: String::new(), tours: Vec::new(), own_favicon: false, extra: Default::default() },
    };
    let replaced = list.tours.iter().any(|t| t.slug == slug);
    let target = tours.join(slug);
    if replaced {
        let foreign = foreign_files(&target)?;
        if !foreign.is_empty() {
            let shown: Vec<&str> = foreign.iter().take(5).map(String::as_str).collect();
            let more = if foreign.len() > 5 { format!(" and {} more", foreign.len() - 5) } else { String::new() };
            return Err(format!("{} holds files Mythicshot did not write ({}{more}), so it will not be replaced. Move them out of the folder and add the tour again.", address(slug), shown.join(", ")));
        }
    }

    // What is missing of the site, never replacing anything. A new site gets its list first, empty, so a
    // crash part-way leaves a site Mythicshot can add to again.
    let mut next = list.clone();
    if !list_path.exists() {
        let empty = serde_json::to_string_pretty(&list).map_err(|e| format!("Could not write tours.json: {e}"))?;
        session.add_if_missing(site_dir, "tours.json", empty.as_bytes())?;
    }
    session.ensure_dir(&tours)?;
    session.ensure_dir(&assets_dir)?;
    let assets = assets();
    session.add_if_missing(&assets_dir, &assets.js_name, assets.js.as_bytes())?;
    session.add_if_missing(&assets_dir, &assets.css_name, assets.css.as_bytes())?;
    session.add_if_missing(site_dir, NOJEKYLL, b"")?;
    // The icon: one the folder already has is left alone and used; ours is written only when there is none.
    if site_dir.join(FAVICON).symlink_metadata().is_err() {
        session.add_if_missing(site_dir, FAVICON, &favicon())?;
        next.own_favicon = true;
    }

    // The tour, complete, in a folder of its own. Removed again on any failure or panic until it is in place.
    let new = temp_name("tmp", slug, &tours);
    session.steps.push(Step::Tree(new.clone()));
    let mut tour = write(&new, &assets)?;
    if !new.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        return Err("The tour export did not write its folder.".to_string());
    }

    // tours.json and index.html, staged before anything visible changes.
    next.site = super::SITE_VERSION;
    next.generator = generator();
    next.title = site_title(title_of_site);
    let mut entry = TourEntry { slug: slug.to_string(), title: crate::tour::tour_title(tour_title), steps: u32::try_from(tour.steps).unwrap_or(u32::MAX), image: THUMB.to_string(), updated: now.to_string(), extra: Default::default() };
    match next.tours.iter_mut().find(|t| t.slug == slug) {
        Some(t) => {
            entry.extra = std::mem::take(&mut t.extra);
            *t = entry;
        }
        None => next.tours.push(entry),
    }
    let list_json = serde_json::to_string_pretty(&next).map_err(|e| format!("Could not write tours.json: {e}"))?;
    let list_tmp = stage(site_dir, list_json.as_bytes(), "tours.json")?;
    let index_tmp = stage(site_dir, index_page(&next).as_bytes(), "index.html")?;
    let old_list = std::fs::read(&list_path).map_err(|e| format!("Could not read tours.json: {e}"))?;

    // The swap.
    let old = if replaced {
        let old = temp_name("old", slug, &tours);
        retry(|| std::fs::rename(&target, &old)).map_err(|e| format!("Could not replace {}: {e}. A file in it may be open in another program; the site is unchanged.", address(slug)))?;
        Some(old)
    } else {
        None
    };
    if let Err(e) = retry(|| std::fs::rename(&new, &target)) {
        return Err(after_unswap(format!("Could not move the tour into {}: {e}", address(slug)), unswap(&tours, slug, false, old.as_deref())));
    }
    if let Err(e) = commit(list_tmp, &list_path, "tours.json") {
        return Err(after_unswap(e, unswap(&tours, slug, true, old.as_deref())));
    }
    if let Err(e) = commit(index_tmp, &index_path, "index.html") {
        if let Err(r) = stage(site_dir, &old_list, "tours.json").and_then(|t| commit(t, &list_path, "tours.json")) {
            eprintln!("Add to site: could not put tours.json back: {r}");
        }
        return Err(after_unswap(e, unswap(&tours, slug, true, old.as_deref())));
    }

    // The site is complete; the old copy goes last, and failing to remove it only earns a note.
    if let Some(old) = &old
        && let Err(e) = remove_ours(old)
    {
        notes.push(format!("The previous version of {} could not be removed ({e}); it is in tours/{}/ and is removed by the next add.", address(slug), shown(old)));
    }
    tour.path = target;
    let note = (!notes.is_empty()).then(|| notes.join(" "));
    Ok(SiteSummary { dir: site_dir.to_path_buf(), slug: slug.to_string(), replaced, tours: next.tours.len(), tour, note })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A `write` closure that makes a tour folder with `steps` step files.
    fn fake(steps: usize) -> impl FnOnce(&Path, &Assets) -> Result<TourSummary, String> {
        move |dir: &Path, a: &Assets| {
            std::fs::create_dir(dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("index.html"), format!("<script src=\"../../assets/{}\"></script>", a.js_name)).unwrap();
            std::fs::write(dir.join("tour.json"), format!("{{\"steps\":{steps}}}")).unwrap();
            std::fs::write(dir.join("thumb.jpg"), "jpg").unwrap();
            for i in 1..=steps {
                std::fs::write(dir.join(format!("step-{i:03}.png")), format!("png {i} {steps}")).unwrap();
            }
            Ok(TourSummary { path: dir.to_path_buf(), steps, bytes: 3 * steps as u64, squeezed: 0 })
        }
    }

    /// Every file and folder under `dir` with its bytes (folders as None).
    fn snapshot(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        let mut out = BTreeMap::new();
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                if p.is_dir() {
                    out.insert(rel, None);
                    walk(base, &p, out);
                } else {
                    out.insert(rel, Some(std::fs::read(&p).unwrap()));
                }
            }
        }
        walk(dir, dir, &mut out);
        out
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn listed(dir: &Path) -> Vec<String> {
        let Folder::Site(l) = inspect(dir).unwrap() else { panic!("not a site") };
        l.tours.into_iter().map(|t| t.slug).collect()
    }

    /// The site as a crash between the two renames of a replace of `slug` leaves it.
    fn crash_between_renames(site: &Path, slug: &str) -> (PathBuf, PathBuf) {
        let tours = site.join("tours");
        let old = tours.join(format!("old-{slug}-0123abcd"));
        std::fs::rename(tours.join(slug), &old).unwrap();
        let new = tours.join(format!("tmp-{slug}-89abcdef"));
        fake(7)(&new, &assets()).unwrap();
        (old, new)
    }

    const T0: &str = "2026-11-17T22:15:00Z";
    const T1: &str = "2026-11-17T22:16:00Z";

    #[test]
    fn the_first_tour_makes_a_site_and_leaves_other_files_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        std::fs::write(docs.join("README.md"), "keep me").unwrap();
        std::fs::write(docs.join("CNAME"), "example").unwrap();
        let s = add_tour(&docs, "Guides", "raid-groups", "Raid groups", T0, fake(3)).unwrap();
        assert_eq!((s.slug.as_str(), s.replaced, s.tours, s.note.as_deref()), ("raid-groups", false, 1, None));
        assert_eq!(s.tour.path, docs.join("tours").join("raid-groups"));
        for f in ["index.html", "tours.json", ".nojekyll", "favicon.ico", "tours/raid-groups/index.html", "tours/raid-groups/thumb.jpg"] {
            assert!(docs.join(f).exists(), "{f}");
        }
        assert_eq!(std::fs::read_dir(docs.join("assets")).unwrap().count(), 2);
        assert_eq!(std::fs::read_to_string(docs.join("README.md")).unwrap(), "keep me");
        assert_eq!(std::fs::read_to_string(docs.join("CNAME")).unwrap(), "example");
        let Folder::Site(l) = inspect(&docs).unwrap() else { panic!() };
        assert_eq!((l.title.as_str(), l.tours[0].steps, l.tours[0].image.as_str(), l.own_favicon), ("Guides", 3, "thumb.jpg", true));
        assert_eq!(l.tours[0].updated, T0);
        // No temporary file or folder, and no lock, is left.
        assert_eq!(names(&docs), vec![".nojekyll", "CNAME", "README.md", "assets", "favicon.ico", "index.html", "tours", "tours.json"]);
        assert_eq!(names(&docs.join("tours")), vec!["raid-groups"]);
        // A folder that does not exist yet is made, and only its last part.
        let fresh = tmp.path().join("fresh");
        add_tour(&fresh, "", "a", "A", T0, fake(1)).unwrap();
        let Folder::Site(l) = inspect(&fresh).unwrap() else { panic!() };
        assert_eq!(l.title, "Tours");
        assert!(add_tour(&tmp.path().join("no").join("site"), "", "a", "A", T0, fake(1)).is_err());
        assert!(!tmp.path().join("no").exists());
    }

    #[test]
    fn a_folder_with_its_own_icon_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(FAVICON), "their icon").unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(1)).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join(FAVICON)).unwrap(), "their icon");
        let Folder::Site(l) = inspect(tmp.path()).unwrap() else { panic!() };
        assert!(!l.own_favicon);
        // Removed later, ours takes its place and is recorded as ours.
        std::fs::remove_file(tmp.path().join(FAVICON)).unwrap();
        add_tour(tmp.path(), "G", "b", "B", T1, fake(1)).unwrap();
        let Folder::Site(l) = inspect(tmp.path()).unwrap() else { panic!() };
        assert!(l.own_favicon && std::fs::read(tmp.path().join(FAVICON)).unwrap() == favicon());
        // And it stays recorded as ours on the next add.
        add_tour(tmp.path(), "G", "c", "C", T1, fake(1)).unwrap();
        let Folder::Site(l) = inspect(tmp.path()).unwrap() else { panic!() };
        assert!(l.own_favicon);
    }

    #[test]
    fn the_icon_is_a_small_plain_ico() {
        let ico = favicon();
        assert_eq!(&ico[..4], &[0, 0, 1, 0]);
        assert_eq!(&ico[22..26], b"\x89PNG");
        let img = image::load_from_memory(&ico[22..]).unwrap();
        assert_eq!((img.width(), img.height()), (32, 32));
        assert!(ico.len() < 2048, "{}", ico.len());
        assert_eq!(favicon(), ico);
    }

    #[test]
    fn a_second_tour_does_not_touch_the_first() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let before = snapshot(&tmp.path().join("tours/a"));
        let s = add_tour(tmp.path(), "G", "b", "B", T1, fake(4)).unwrap();
        assert_eq!(s.tours, 2);
        assert_eq!(snapshot(&tmp.path().join("tours/a")), before);
        let index = std::fs::read_to_string(tmp.path().join("index.html")).unwrap();
        assert!(index.contains("tours/b/index.html") && index.contains("tours/a/index.html") && index.contains("2 tours"));
    }

    #[test]
    fn replacing_keeps_the_tours_place_and_leaves_no_temporary_folder() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        add_tour(tmp.path(), "G", "b", "B", T1, fake(2)).unwrap();
        let s = add_tour(tmp.path(), "G2", "a", "A again", "2026-11-17T22:17:00Z", fake(5)).unwrap();
        assert!(s.replaced);
        let Folder::Site(l) = inspect(tmp.path()).unwrap() else { panic!() };
        assert_eq!(l.tours.iter().map(|t| (t.slug.as_str(), t.steps)).collect::<Vec<_>>(), vec![("a", 5), ("b", 2)]);
        assert_eq!((l.title.as_str(), l.tours[0].title.as_str(), l.tours[0].updated.as_str()), ("G2", "A again", "2026-11-17T22:17:00Z"));
        assert_eq!(names(&tmp.path().join("tours")), vec!["a", "b"]);
        assert_eq!(names(&tmp.path().join("tours/a")).len(), 3 + 5);
        assert_eq!(std::fs::read_to_string(tmp.path().join("tours/a/tour.json")).unwrap(), "{\"steps\":5}");
        assert!(names(tmp.path()).iter().all(|n| !n.ends_with(".tmp") && n != LOCK), "{:?}", names(tmp.path()));
    }

    #[test]
    fn fields_added_by_hand_survive_a_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join("tours.json")).unwrap()).unwrap();
        v["theme"] = serde_json::json!({ "accent": "#123456" });
        v["tours"][0]["tags"] = serde_json::json!(["raid"]);
        std::fs::write(tmp.path().join("tours.json"), serde_json::to_string_pretty(&v).unwrap()).unwrap();
        add_tour(tmp.path(), "G", "b", "B", T1, fake(1)).unwrap();
        add_tour(tmp.path(), "G", "a", "A again", T1, fake(3)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join("tours.json")).unwrap()).unwrap();
        assert_eq!(v["theme"]["accent"], "#123456");
        assert_eq!((v["tours"][0]["slug"].as_str(), v["tours"][0]["tags"][0].as_str(), v["tours"][0]["steps"].as_u64()), (Some("a"), Some("raid"), Some(3)));
        assert!(v["tours"][1].get("tags").is_none());
    }

    #[test]
    fn a_tour_folder_holding_other_files_is_not_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        std::fs::write(tmp.path().join("tours/a/notes.txt"), "mine").unwrap();
        std::fs::create_dir(tmp.path().join("tours/a/extra")).unwrap();
        let before = snapshot(tmp.path());
        let err = add_tour(tmp.path(), "G", "a", "A", T1, fake(3)).unwrap_err();
        assert!(err.contains("tours/a/") && err.contains("notes.txt") && err.contains("extra/"), "{err}");
        assert_eq!(snapshot(tmp.path()), before);
        // What the export writes is all allowed.
        for ok in ["index.html", "tour.json", "thumb.jpg", "step-001.png", "step-200.jpg", "logo.png", "logo.jpg"] {
            assert!(tour_file(ok), "{ok}");
        }
        for bad in ["notes.txt", "step-1.png", "step-001.gif", "logo.svg", "Index.html", "step-abc.png", ".hidden"] {
            assert!(!tour_file(bad), "{bad}");
        }
    }

    #[test]
    fn a_failed_export_leaves_the_old_tour_and_the_site_as_they_were() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let before = snapshot(tmp.path());
        let failing = |dir: &Path, _: &Assets| {
            std::fs::create_dir(dir).unwrap();
            std::fs::write(dir.join("step-001.png"), "half").unwrap();
            Err("disk full".to_string())
        };
        assert_eq!(add_tour(tmp.path(), "G", "a", "A", T1, failing).unwrap_err(), "disk full");
        assert_eq!(snapshot(tmp.path()), before);
        // A new tour that fails, or is cancelled, leaves nothing of itself either.
        let failing = |_: &Path, _: &Assets| Err("cancelled".to_string());
        assert_eq!(add_tour(tmp.path(), "Other", "new", "N", T1, failing).unwrap_err(), "cancelled");
        assert_eq!(snapshot(tmp.path()), before);
    }

    #[test]
    fn a_panic_in_the_export_still_undoes_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = tmp.path().join("fresh");
        let panicking = |dir: &Path, _: &Assets| -> Result<TourSummary, String> {
            std::fs::create_dir(dir).unwrap();
            std::fs::write(dir.join("step-001.png"), "half").unwrap();
            panic!("boom")
        };
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| add_tour(&fresh, "G", "a", "A", T0, panicking))).is_err());
        assert!(!fresh.exists(), "no skeleton is left");
        // In an existing site: no temporary folder, no lock, and the site can be added to at once.
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let before = snapshot(tmp.path());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| add_tour(tmp.path(), "G", "a", "A", T1, panicking))).is_err());
        assert_eq!(snapshot(tmp.path()), before);
        add_tour(tmp.path(), "G", "b", "B", T1, fake(1)).unwrap();
    }

    #[test]
    fn a_failed_first_tour_leaves_the_folder_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        std::fs::write(docs.join("README.md"), "keep me").unwrap();
        let before = snapshot(&docs);
        assert!(add_tour(&docs, "G", "a", "A", T0, |_: &Path, _: &Assets| Err("cancelled".to_string())).is_err());
        assert_eq!(snapshot(&docs), before);
        let fresh = tmp.path().join("fresh");
        assert!(add_tour(&fresh, "G", "a", "A", T0, |_: &Path, _: &Assets| Err("cancelled".to_string())).is_err());
        assert!(!fresh.exists());
    }

    #[test]
    fn an_interrupted_replace_is_put_back_by_the_next_add() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        add_tour(tmp.path(), "G", "b", "B", T0, fake(2)).unwrap();
        let before_a = snapshot(&tmp.path().join("tours/a"));
        crash_between_renames(tmp.path(), "a");
        // An add at another address puts tours/a/ back from its earlier copy and clears the half-done new one.
        let s = add_tour(tmp.path(), "G", "c", "C", T1, fake(1)).unwrap();
        assert!(s.note.as_deref().unwrap_or("").contains("tours/a/"), "{:?}", s.note);
        assert_eq!(snapshot(&tmp.path().join("tours/a")), before_a);
        assert_eq!(names(&tmp.path().join("tours")), vec!["a", "b", "c"]);
        assert_eq!(listed(tmp.path()), vec!["a", "b", "c"]);
        // The same address works too.
        crash_between_renames(tmp.path(), "b");
        let s = add_tour(tmp.path(), "G", "b", "B again", T1, fake(4)).unwrap();
        assert!(s.replaced);
        assert_eq!(names(&tmp.path().join("tours")), vec!["a", "b", "c"]);
        assert_eq!(std::fs::read_to_string(tmp.path().join("tours/b/tour.json")).unwrap(), "{\"steps\":4}");
    }

    #[test]
    fn an_ambiguous_interrupted_replace_is_refused_with_the_folders_named() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let (old, _) = crash_between_renames(tmp.path(), "a");
        let second = tmp.path().join("tours/old-a-fedcba98");
        std::fs::create_dir(&second).unwrap();
        let before = snapshot(tmp.path());
        let err = add_tour(tmp.path(), "G", "z", "Z", T1, fake(1)).unwrap_err();
        assert!(err.contains("tours/a/") && err.contains(&format!("tours/{}/", shown(&old))) && err.contains("tours/old-a-fedcba98/") && err.contains("Rename"), "{err}");
        assert_eq!(snapshot(tmp.path()), before);
    }

    #[test]
    fn a_failed_put_back_keeps_both_copies_and_says_where_they_are() {
        let tmp = tempfile::tempdir().unwrap();
        let tours = tmp.path().join("tours");
        std::fs::create_dir(&tours).unwrap();
        // The new version is in place and the previous one aside; putting it back fails because its folder is gone.
        fake(3)(&tours.join("a"), &assets()).unwrap();
        let missing_old = tours.join("old-a-00000000");
        let err = unswap(&tours, "a", true, Some(&missing_old)).unwrap_err();
        assert!(err.contains("tours/a/ is missing") && err.contains("tours/old-a-00000000/") && err.contains("the new one in tours/tmp-a-"), "{err}");
        // The new copy was kept, not deleted.
        let kept: Vec<String> = names(&tours).into_iter().filter(|n| n.starts_with("tmp-a-")).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(names(&tours.join(&kept[0])).len(), 6);
        // The message a caller makes says nothing about the site being unchanged.
        let msg = after_unswap("Could not write tours.json: disk full".into(), Err(err));
        assert!(!msg.contains("unchanged") && msg.contains("tours/old-a-00000000/"), "{msg}");
        assert_eq!(after_unswap("x".into(), Ok(())), "x. The site is unchanged.");
    }

    #[test]
    fn leftovers_of_every_address_are_cleaned_up_but_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        add_tour(tmp.path(), "G", "a-b", "A b", T0, fake(2)).unwrap();
        let tours = tmp.path().join("tours");
        for d in ["tmp-a-0123abcd", "old-a-89abcdef", "tmp-a-b-01234567", "old-zz-01234567", "tmp-a-89ABCDEF", "tmp-a-0123abc", "tmp--01234567", "keep"] {
            std::fs::create_dir(tours.join(d)).unwrap();
        }
        std::fs::write(tours.join("old-a-00000000"), "a file, not a folder").unwrap();
        add_tour(tmp.path(), "G", "c", "C", T1, fake(1)).unwrap();
        assert_eq!(names(&tours), vec!["a", "a-b", "c", "keep", "old-a-00000000", "tmp--01234567", "tmp-a-0123abc", "tmp-a-89ABCDEF"]);
        assert!(leftover_of("tmp-a-0123abcd", "a") && leftover_of("old-a-b-0123abcd", "a-b"));
        assert!(!leftover_of("tmp-a-b-0123abcd", "a") && !leftover_of("tmp-ab-0123abcd", "a") && !leftover_of("a", "a"));
        assert_eq!(temp_owner("old-zz-01234567"), Some(("old", "zz")));
        assert_eq!(temp_owner("tmp-assets-01234567"), None);
    }

    #[test]
    fn a_tour_listed_under_a_temporary_name_by_hand_survives() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("tours.json")).unwrap();
        let mut list: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut odd = list["tours"][0].clone();
        odd["slug"] = "old-a-1234abcd".into();
        list["tours"].as_array_mut().unwrap().push(odd);
        std::fs::write(tmp.path().join("tours.json"), serde_json::to_string_pretty(&list).unwrap()).unwrap();
        std::fs::create_dir(tmp.path().join("tours/old-a-1234abcd")).unwrap();
        std::fs::write(tmp.path().join("tours/old-a-1234abcd/index.html"), "a real tour").unwrap();
        let before = snapshot(tmp.path());
        assert!(add_tour(tmp.path(), "G", "a", "A", T1, fake(1)).unwrap_err().contains("tours.json"));
        assert_eq!(snapshot(tmp.path()), before);
        // And neither clean-up nor recovery ever takes a listed name, whatever the list holds.
        let entry = |slug: &str| TourEntry { slug: slug.into(), title: "x".into(), steps: 1, image: THUMB.into(), updated: T0.into(), extra: Default::default() };
        let l = SiteList { site: 1, generator: "mythicshot".into(), title: "G".into(), tours: vec![entry("old-a-1234abcd")], own_favicon: true, extra: Default::default() };
        sweep(&tmp.path().join("tours"), &l);
        assert_eq!(std::fs::read_to_string(tmp.path().join("tours/old-a-1234abcd/index.html")).unwrap(), "a real tour");
    }

    #[test]
    fn stale_staged_files_are_removed_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(1)).unwrap();
        let ten_minutes_ago = SystemTime::now() - Duration::from_secs(600);
        let make = |rel: &str, old: bool| {
            let p = tmp.path().join(rel);
            std::fs::write(&p, "x").unwrap();
            if old {
                std::fs::File::options().write(true).open(&p).unwrap().set_modified(ten_minutes_ago).unwrap();
            }
        };
        make(".mythicshot-a1B2c3.tmp", true);
        make("assets/.mythicshot-zz99zz.tmp", true);
        make(".mythicshot-fresh1.tmp", false);
        make(".mythicshot-toolong7.tmp", true);
        make("mythicshot-a1b2c3.tmp", true);
        make("notes.tmp", true);
        add_tour(tmp.path(), "G", "b", "B", T1, fake(1)).unwrap();
        let top = names(tmp.path());
        assert!(!top.contains(&".mythicshot-a1B2c3.tmp".to_string()) && !names(&tmp.path().join("assets")).contains(&".mythicshot-zz99zz.tmp".to_string()));
        for kept in [".mythicshot-fresh1.tmp", ".mythicshot-toolong7.tmp", "mythicshot-a1b2c3.tmp", "notes.tmp"] {
            assert!(top.contains(&kept.to_string()), "{kept}");
        }
    }

    #[test]
    fn one_add_at_a_time_per_site() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(1)).unwrap();
        let site = tmp.path().to_path_buf();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let first = {
            let site = site.clone();
            std::thread::spawn(move || {
                add_tour(&site, "G", "b", "B", T1, move |dir: &Path, a: &Assets| {
                    started_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                    fake(2)(dir, a)
                })
            })
        };
        started_rx.recv().unwrap();
        // While the first add runs, a second one, at any address and by any spelling of the folder, is told to wait.
        assert_eq!(add_tour(&site, "G", "c", "C", T1, fake(1)).unwrap_err(), BUSY);
        assert_eq!(add_tour(&site.join("tours").join(".."), "G", "b", "B", T1, fake(1)).unwrap_err(), BUSY);
        go_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        // Afterwards the second add works, and both tours are listed.
        add_tour(&site, "G", "c", "C", T1, fake(1)).unwrap();
        assert_eq!(listed(&site), vec!["a", "b", "c"]);
        assert!(!site.join(LOCK).exists());
    }

    #[test]
    fn another_programs_lock_is_respected_until_its_owner_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(1)).unwrap();
        // A running process holds the site.
        #[cfg(windows)]
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        #[cfg(not(windows))]
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::write(tmp.path().join(LOCK), format!("pid {}\ntime {}\n", child.id(), now_secs())).unwrap();
        let before = snapshot(tmp.path());
        assert_eq!(add_tour(tmp.path(), "G", "b", "B", T1, fake(1)).unwrap_err(), BUSY);
        assert_eq!(snapshot(tmp.path()), before);
        // Once it has gone, its lock is stale and is replaced, then removed.
        child.kill().unwrap();
        child.wait().unwrap();
        add_tour(tmp.path(), "G", "b", "B", T1, fake(1)).unwrap();
        assert!(!tmp.path().join(LOCK).exists());
        // A lock that is very old is stale whoever wrote it; this process's own is a leftover; a fresh unreadable one is not.
        assert!(lock_is_stale(&format!("pid {}\ntime 0\n", 1), None));
        assert!(lock_is_stale(&format!("pid {}\ntime {}\n", std::process::id(), now_secs()), None));
        assert!(!lock_is_stale("", Some(SystemTime::now())));
        assert!(lock_is_stale("", Some(SystemTime::now() - Duration::from_secs(120))));
        // A lock that is a folder is not removed.
        std::fs::create_dir(tmp.path().join(LOCK)).unwrap();
        assert!(add_tour(tmp.path(), "G", "c", "C", T1, fake(1)).unwrap_err().contains(LOCK));
        assert!(tmp.path().join(LOCK).is_dir());
    }

    #[test]
    fn quitting_waits_for_adds_in_flight() {
        let flight = InFlight::new();
        let start = Instant::now();
        wait_for_adds(Duration::from_millis(200));
        assert!(start.elapsed() >= Duration::from_millis(190));
        drop(flight);
    }

    #[test]
    fn drives_and_main_folders_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(check_dedicated(tmp.path()).is_ok());
        assert!(check_dedicated(&tmp.path().join("not-yet")).is_ok());
        let root = tmp.path().ancestors().last().unwrap();
        assert!(check_dedicated(root).unwrap_err().contains("drive"), "{root:?}");
        let user = directories::UserDirs::new().unwrap();
        let main = [Some(user.home_dir()), user.desktop_dir(), user.document_dir(), user.download_dir()];
        for d in main.into_iter().flatten().filter(|d| d.is_dir()) {
            let err = check_dedicated(d).unwrap_err();
            assert!(err.contains("folder just for the site"), "{d:?}: {err}");
            // The add refuses before anything is written there, and a folder inside is fine to check.
            let before = names(d);
            assert!(add_tour(d, "G", "a", "A", T0, fake(1)).is_err());
            assert_eq!(names(d), before);
            assert!(check_dedicated(&d.join("mythicshot-site-not-made")).is_ok());
        }
    }

    #[test]
    fn folders_this_feature_did_not_make_are_refused_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        std::fs::create_dir(tmp.path().join("tours/mine")).unwrap();
        std::fs::write(tmp.path().join("tours/mine/notes.txt"), "x").unwrap();
        std::fs::create_dir(tmp.path().join("tours/Theirs")).unwrap();
        let before = snapshot(tmp.path());
        let err = add_tour(tmp.path(), "G", "mine", "Mine", T1, fake(1)).unwrap_err();
        assert!(err.contains("tours/mine"), "{err}");
        assert_eq!(snapshot(tmp.path()), before);
        let err = add_tour(tmp.path(), "G", "theirs", "Theirs", T1, fake(1)).unwrap_err();
        assert!(err.contains("tours/theirs"), "{err}");
        assert_eq!(snapshot(tmp.path()), before);
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("index.html"), "theirs").unwrap();
        assert!(add_tour(other.path(), "G", "a", "A", T0, fake(1)).is_err());
        assert_eq!(std::fs::read_to_string(other.path().join("index.html")).unwrap(), "theirs");
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 1);
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("assets"), "theirs").unwrap();
        let err = add_tour(other.path(), "G", "a", "A", T0, fake(1)).unwrap_err();
        assert!(err.contains("assets"), "{err}");
        assert_eq!(names(other.path()), vec!["assets".to_string()]);
        let elsewhere = tempfile::tempdir().unwrap();
        let file = elsewhere.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(add_tour(&file, "G", "a", "A", T0, fake(1)).is_err());
        for bad in ["../x", "a/b", "", "A", "con", "assets", "tmp-a-1234abcd", "old-a-1234abcd"] {
            assert!(add_tour(tmp.path(), "G", bad, "A", T1, fake(1)).is_err(), "{bad}");
        }
        assert_eq!(snapshot(tmp.path()), before);
    }

    #[test]
    fn an_entry_whose_folder_is_missing_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        std::fs::remove_dir_all(tmp.path().join("tours/a")).unwrap();
        let before = snapshot(tmp.path());
        let err = add_tour(tmp.path(), "G", "b", "B", T1, fake(1)).unwrap_err();
        assert!(err.contains("tours.json") && err.contains("tours/a"), "{err}");
        assert_eq!(snapshot(tmp.path()), before);
    }

    #[test]
    fn a_damaged_tours_json_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        std::fs::write(tmp.path().join("tours.json"), "{ not json").unwrap();
        let before = snapshot(tmp.path());
        assert!(add_tour(tmp.path(), "G", "b", "B", T1, fake(1)).unwrap_err().contains("tours.json"));
        assert_eq!(snapshot(tmp.path()), before);
    }

    #[test]
    fn shared_files_are_added_never_overwritten_and_nothing_names_this_machine() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let a = assets();
        std::fs::write(tmp.path().join("assets").join(&a.js_name), "edited by hand").unwrap();
        std::fs::write(tmp.path().join("assets/player.0000000000000000.js"), "an older player").unwrap();
        std::fs::write(tmp.path().join("favicon.ico"), "my own icon").unwrap();
        std::fs::write(tmp.path().join(".nojekyll"), "kept").unwrap();
        add_tour(tmp.path(), "G", "b", "B", T1, fake(2)).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join("assets").join(&a.js_name)).unwrap(), "edited by hand");
        assert!(tmp.path().join("assets/player.0000000000000000.js").exists());
        assert_eq!(std::fs::read_to_string(tmp.path().join("favicon.ico")).unwrap(), "my own icon");
        assert_eq!(std::fs::read_to_string(tmp.path().join(".nojekyll")).unwrap(), "kept");
        let here = tmp.path().to_string_lossy().replace('\\', "/");
        let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
        for f in ["index.html", "tours.json"] {
            let text = std::fs::read_to_string(tmp.path().join(f)).unwrap().replace('\\', "/");
            assert!(!text.contains(&here) && !text.contains(":/"), "{f}");
            assert!(user.len() < 3 || !text.to_lowercase().contains(&user.to_lowercase()), "{f}");
        }
    }

    #[test]
    fn a_link_inside_the_site_is_refused_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        add_tour(&site, "G", "a", "A", T0, fake(2)).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "x").unwrap();
        std::fs::remove_dir_all(site.join("tours/a")).unwrap();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, site.join("tours/a"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, site.join("tours/a"));
        if made.is_err() {
            // Creating links needs a privilege Windows does not give every account.
            return;
        }
        assert!(add_tour(&site, "G", "a", "A", T1, fake(1)).unwrap_err().contains("link"));
        assert!(add_tour(&site, "G", "b", "B", T1, fake(1)).unwrap_err().contains("link"));
        assert_eq!(names(&outside), vec!["keep.txt"]);
    }

    #[test]
    fn a_write_that_does_not_make_its_folder_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let before = snapshot(tmp.path());
        let nothing = |_: &Path, _: &Assets| Ok(TourSummary { path: PathBuf::new(), steps: 1, bytes: 0, squeezed: 0 });
        assert!(add_tour(tmp.path(), "G", "a", "A", T1, nothing).is_err());
        assert_eq!(snapshot(tmp.path()), before);
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_file_in_the_old_tour_fails_cleanly() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        let before = snapshot(tmp.path());
        // Open as a browser or a scanner might: others may read, but not delete or rename.
        let lock = std::fs::OpenOptions::new().read(true).share_mode(1).open(tmp.path().join("tours/a/tour.json")).unwrap();
        let started = Instant::now();
        let err = add_tour(tmp.path(), "G", "a", "A again", T1, fake(5)).unwrap_err();
        assert!(err.contains("tours/a/") && err.contains("unchanged"), "{err}");
        assert!(started.elapsed() >= RETRY_FOR / 2, "it was tried again before giving up");
        assert_eq!(std::fs::read_to_string(tmp.path().join("tours/a/tour.json")).unwrap(), "{\"steps\":2}");
        drop(lock);
        assert_eq!(snapshot(tmp.path()), before);
        assert!(add_tour(tmp.path(), "G", "a", "A again", T1, fake(5)).unwrap().replaced);
    }

    #[test]
    fn busy_files_are_tried_again_then_given_up() {
        let mut n = 0;
        let r: std::io::Result<()> = retry(|| {
            n += 1;
            if n < 3 { Err(std::io::Error::from_raw_os_error(32)) } else { Ok(()) }
        });
        if cfg!(windows) {
            assert!(r.is_ok() && n == 3);
        } else {
            assert!(r.is_err() && n == 1);
        }
        let mut n = 0;
        let r: std::io::Result<()> = retry(|| {
            n += 1;
            Err(std::io::Error::other("not busy"))
        });
        assert!(r.is_err() && n == 1);
    }
}
