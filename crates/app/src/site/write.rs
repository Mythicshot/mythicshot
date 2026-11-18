//! Adding a tour to a site folder, or replacing one, so that the site works at every moment.
//!
//! The new tour is written into a temporary folder beside the others and swapped into place only when it
//! is complete; `tours.json` and the index follow, each through a temporary file and a rename. Any failure
//! before the end puts back what was there and removes what this call made. Nothing this feature did not
//! create is ever deleted or overwritten, and no link is followed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use image::ImageEncoder as _;

use super::{Assets, Folder, SiteList, THUMB, TourEntry, address, assets, check_slug, index_page, inspect, site_title};
use crate::tour::TourSummary;

/// The site's icon, so browsers asking for one get it instead of a missing file.
pub const FAVICON: &str = "favicon.ico";
/// An empty marker some hosts read as "serve these files as they are".
const NOJEKYLL: &str = ".nojekyll";

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
    /// Something that did not work out after the site was already complete, such as an old copy left behind.
    pub note: Option<String>,
}

/// How long a rename or removal that Windows refuses because a file is in use is tried again.
#[cfg(not(test))]
const RETRY_FOR: Duration = Duration::from_secs(2);
#[cfg(test)]
const RETRY_FOR: Duration = Duration::from_millis(300);

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

/// Eight lowercase hex digits, different every call.
fn hex8() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
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

/// Whether `name` is exactly a temporary folder name this feature gives `slug`.
fn leftover_of(name: &str, slug: &str) -> bool {
    ["tmp-", "old-"].iter().any(|kind| {
        name.strip_prefix(kind).and_then(|r| r.strip_prefix(slug)).and_then(|r| r.strip_prefix('-')).is_some_and(|hex| hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    })
}

/// The generator recorded in `tours.json`.
fn generator() -> String {
    format!("mythicshot {}", env!("CARGO_PKG_VERSION"))
}

/// What this call created, removed again in reverse order if it fails. Folders are removed only when empty.
#[derive(Default)]
struct Made(Vec<(PathBuf, bool)>);

impl Made {
    fn undo(&mut self) {
        for (path, folder) in self.0.drain(..).rev() {
            let r = if folder { std::fs::remove_dir(&path) } else { retry(|| std::fs::remove_file(&path)) };
            if let Err(e) = r {
                eprintln!("Add to site: could not remove {}: {e}", path.display());
            }
        }
    }
}

/// Create the folder `dir` unless it is there. Only its last part is created.
fn ensure_dir(dir: &Path, made: &mut Made) -> Result<(), String> {
    match std::fs::create_dir(dir) {
        Ok(()) => {
            made.0.push((dir.to_path_buf(), true));
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.symlink_metadata().is_ok_and(|m| m.is_dir()) => Ok(()),
        Err(e) => Err(format!("Could not create the folder {}: {e}", shown(dir))),
    }
}

/// The last part of a path, for messages that do not show where the site is.
fn shown(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

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

/// Add `name` with `bytes` in `dir` only if nothing has that name; never replaces anything.
fn add_if_missing(dir: &Path, name: &str, bytes: &[u8], made: &mut Made) -> Result<(), String> {
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
            made.0.push((target, false));
            Ok(())
        }
        // Something of that name appeared in the meantime: it stays as it is.
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("Could not write {name}: {}", e.error)),
    }
}

/// Remove a folder this call made (by its unique temporary name), trying again while a file in it is busy.
fn remove_ours(dir: &Path) -> std::io::Result<()> {
    match dir.symlink_metadata() {
        Ok(m) if m.is_dir() => retry(|| std::fs::remove_dir_all(dir)),
        Ok(_) => retry(|| std::fs::remove_file(dir)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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

/// Remove folders an earlier failed run left for this slug: only real folders whose name is exactly a
/// temporary name for it and that no tour in the list has. A failure is only reported.
fn sweep(tours: &Path, slug: &str, list: &SiteList) {
    let Ok(entries) = std::fs::read_dir(tours) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !leftover_of(&name, slug) || list.tours.iter().any(|t| t.slug == name) {
            continue;
        }
        if e.path().symlink_metadata().is_ok_and(|m| m.is_dir())
            && let Err(err) = retry(|| std::fs::remove_dir_all(e.path()))
        {
            eprintln!("Add to site: could not remove the leftover folder tours/{name}: {err}");
        }
    }
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


/// Add the tour to the site in `site_dir`, or replace the one at `slug`.
/// `write(new_dir, assets)` must create `new_dir` and write the tour into it (it is `write_tour` with `Page::Site`).
/// `now` is the UTC time for the entry. Nothing outside `site_dir` is touched.
///
/// The order: check everything; create what is missing of the site (the folder itself, `tours`, `assets`,
/// the shared player files, `.nojekyll`, the icon), never replacing anything; remove this slug's leftovers
/// from an earlier failed run; write the tour into `tours/tmp-<slug>-<8 hex>`; move the old tour aside to
/// `tours/old-<slug>-<8 hex>` and the new one into place; write `tours.json`, then `index.html`; remove the
/// old tour. A failure before the end leaves the site as it was, byte for byte, and removes what this call made.
pub fn add_tour(site_dir: &Path, title_of_site: &str, slug: &str, tour_title: &str, now: &str, write: impl FnOnce(&Path, &Assets) -> Result<TourSummary, String>) -> Result<SiteSummary, String> {
    check_slug(slug)?;
    let folder = inspect(site_dir)?;
    let tours = site_dir.join("tours");
    let list = match folder {
        Folder::Site(list) => {
            check_tours(&tours, &list, slug)?;
            list
        }
        Folder::New => SiteList { site: super::SITE_VERSION, generator: generator(), title: String::new(), tours: Vec::new() },
    };
    let mut made = Made::default();
    let result = add_checked(site_dir, &tours, list, title_of_site, slug, tour_title, now, write, &mut made);
    if result.is_err() {
        made.undo();
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn add_checked(
    site_dir: &Path,
    tours: &Path,
    list: SiteList,
    title_of_site: &str,
    slug: &str,
    tour_title: &str,
    now: &str,
    write: impl FnOnce(&Path, &Assets) -> Result<TourSummary, String>,
    made: &mut Made,
) -> Result<SiteSummary, String> {
    let assets = assets();
    let list_path = site_dir.join("tours.json");
    let index_path = site_dir.join("index.html");
    ensure_dir(site_dir, made)?;
    // A new site gets its list first, empty, so a crash part-way leaves a site Mythicshot can add to again.
    if list.tours.is_empty() && list_path.symlink_metadata().is_err() {
        let empty = serde_json::to_string_pretty(&list).map_err(|e| format!("Could not write tours.json: {e}"))?;
        add_if_missing(site_dir, "tours.json", empty.as_bytes(), made)?;
    }
    ensure_dir(tours, made)?;
    let assets_dir = site_dir.join("assets");
    ensure_dir(&assets_dir, made)?;
    add_if_missing(&assets_dir, &assets.js_name, assets.js.as_bytes(), made)?;
    add_if_missing(&assets_dir, &assets.css_name, assets.css.as_bytes(), made)?;
    add_if_missing(site_dir, NOJEKYLL, b"", made)?;
    add_if_missing(site_dir, FAVICON, &favicon(), made)?;
    sweep(tours, slug, &list);

    // The tour, complete, in a folder of its own.
    let new = temp_name("tmp", slug, tours);
    let mut tour = match write(&new, &assets) {
        Ok(t) => t,
        Err(e) => {
            if let Err(err) = remove_ours(&new) {
                eprintln!("Add to site: could not remove tours/{}: {err}", shown(&new));
            }
            return Err(e);
        }
    };
    let discard_new = |why: String| {
        if let Err(err) = remove_ours(&new) {
            eprintln!("Add to site: could not remove tours/{}: {err}", shown(&new));
        }
        why
    };
    if !new.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        return Err(discard_new("The tour export did not write its folder.".to_string()));
    }

    // tours.json and index.html, staged before anything visible changes.
    let mut next = list.clone();
    next.site = super::SITE_VERSION;
    next.generator = generator();
    next.title = site_title(title_of_site);
    let entry = TourEntry { slug: slug.to_string(), title: crate::tour::tour_title(tour_title), steps: u32::try_from(tour.steps).unwrap_or(u32::MAX), image: THUMB.to_string(), updated: now.to_string() };
    let replaced = match next.tours.iter_mut().find(|t| t.slug == slug) {
        Some(t) => {
            *t = entry;
            true
        }
        None => {
            next.tours.push(entry);
            false
        }
    };
    let list_json = serde_json::to_string_pretty(&next).map_err(|e| discard_new(format!("Could not write tours.json: {e}")))?;
    let list_tmp = stage(site_dir, list_json.as_bytes(), "tours.json").map_err(&discard_new)?;
    let index_tmp = stage(site_dir, index_page(&next).as_bytes(), "index.html").map_err(&discard_new)?;
    let old_list = match std::fs::read(&list_path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(discard_new(format!("Could not read tours.json: {e}"))),
    };

    // The swap.
    let target = tours.join(slug);
    let old = if replaced {
        let old = temp_name("old", slug, tours);
        if let Err(e) = retry(|| std::fs::rename(&target, &old)) {
            return Err(discard_new(format!("Could not replace {}: {e}. A file in it may be open in another program; the site is unchanged.", address(slug))));
        }
        Some(old)
    } else {
        None
    };
    // Put the site's tours back as they were before the swap.
    let unswap = |tours_dir: &Path| {
        let aside = temp_name("tmp", slug, tours_dir);
        if target.symlink_metadata().is_ok()
            && let Err(e) = retry(|| std::fs::rename(&target, &aside))
        {
            eprintln!("Add to site: could not move the new {} aside: {e}", address(slug));
        }
        if let Some(old) = &old
            && let Err(e) = retry(|| std::fs::rename(old, &target))
        {
            eprintln!("Add to site: could not put the previous {} back: {e}", address(slug));
        }
        for dir in [&aside, &new] {
            if let Err(e) = remove_ours(dir) {
                eprintln!("Add to site: could not remove tours/{}: {e}", shown(dir));
            }
        }
    };
    if let Err(e) = retry(|| std::fs::rename(&new, &target)) {
        unswap(tours);
        return Err(format!("Could not move the tour into {}: {e}. The site is unchanged.", address(slug)));
    }
    if let Err(e) = commit(list_tmp, &list_path, "tours.json") {
        unswap(tours);
        return Err(format!("{e}. The site is unchanged."));
    }
    if let Err(e) = commit(index_tmp, &index_path, "index.html") {
        let restored = match &old_list {
            Some(bytes) => stage(site_dir, bytes, "tours.json").and_then(|t| commit(t, &list_path, "tours.json")),
            None => retry(|| std::fs::remove_file(&list_path)).map_err(|e| e.to_string()),
        };
        if let Err(r) = restored {
            eprintln!("Add to site: could not put tours.json back: {r}");
        }
        unswap(tours);
        return Err(format!("{e}. The site is unchanged."));
    }

    // The site is complete; the old copy goes last, and failing to remove it only earns a note.
    let note = old.and_then(|old| {
        remove_ours(&old).err().map(|e| format!("The previous version of {} could not be removed ({e}); it is in tours/{} and is removed the next time this tour is added.", address(slug), shown(&old)))
    });
    // Everything this call made is now part of the site.
    made.0.clear();
    tour.path = target;
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
        assert_eq!((l.title.as_str(), l.tours[0].steps, l.tours[0].image.as_str()), ("Guides", 3, "thumb.jpg"));
        assert_eq!(l.tours[0].updated, T0);
        // No temporary file or folder is left.
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
        // No staged file is left beside the site's own files either.
        assert!(names(tmp.path()).iter().all(|n| !n.ends_with(".tmp")), "{:?}", names(tmp.path()));
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
    fn a_failed_first_tour_leaves_the_folder_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        std::fs::write(docs.join("README.md"), "keep me").unwrap();
        let before = snapshot(&docs);
        assert!(add_tour(&docs, "G", "a", "A", T0, |_: &Path, _: &Assets| Err("cancelled".to_string())).is_err());
        assert_eq!(snapshot(&docs), before);
        // A folder that did not exist is not left behind.
        let fresh = tmp.path().join("fresh");
        assert!(add_tour(&fresh, "G", "a", "A", T0, |_: &Path, _: &Assets| Err("cancelled".to_string())).is_err());
        assert!(!fresh.exists());
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
        // The same name in other letters is the same folder on Windows, and is refused everywhere.
        let err = add_tour(tmp.path(), "G", "theirs", "Theirs", T1, fake(1)).unwrap_err();
        assert!(err.contains("tours/theirs"), "{err}");
        assert_eq!(snapshot(tmp.path()), before);
        // A folder with its own index.html and no tours.json is not a site.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("index.html"), "theirs").unwrap();
        assert!(add_tour(other.path(), "G", "a", "A", T0, fake(1)).is_err());
        assert_eq!(std::fs::read_to_string(other.path().join("index.html")).unwrap(), "theirs");
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 1);
        // Nor is one whose icon or assets are someone else's.
        for name in ["favicon.ico", "assets"] {
            let other = tempfile::tempdir().unwrap();
            std::fs::write(other.path().join(name), "theirs").unwrap();
            let err = add_tour(other.path(), "G", "a", "A", T0, fake(1)).unwrap_err();
            assert!(err.contains(name), "{err}");
            assert_eq!(names(other.path()), vec![name.to_string()]);
        }
        // A path that is a file, and addresses that would leave the site, write nothing.
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
    fn only_this_slugs_own_leftovers_are_cleaned_up() {
        let tmp = tempfile::tempdir().unwrap();
        add_tour(tmp.path(), "G", "a", "A", T0, fake(2)).unwrap();
        add_tour(tmp.path(), "G", "a-b", "A b", T0, fake(2)).unwrap();
        let tours = tmp.path().join("tours");
        for d in ["tmp-a-0123abcd", "old-a-89abcdef", "tmp-a-b-01234567", "tmp-a-89ABCDEF", "tmp-a-0123abc", "keep"] {
            std::fs::create_dir(tours.join(d)).unwrap();
        }
        std::fs::write(tours.join("old-a-00000000"), "a file, not a folder").unwrap();
        add_tour(tmp.path(), "G", "a", "A", T1, fake(1)).unwrap();
        assert_eq!(names(&tours), vec!["a", "a-b", "keep", "old-a-00000000", "tmp-a-0123abc", "tmp-a-89ABCDEF", "tmp-a-b-01234567"]);
        assert!(leftover_of("tmp-a-0123abcd", "a") && leftover_of("old-a-b-0123abcd", "a-b"));
        assert!(!leftover_of("tmp-a-b-0123abcd", "a") && !leftover_of("tmp-ab-0123abcd", "a") && !leftover_of("a", "a"));
    }

    #[test]
    fn a_tour_listed_under_a_temporary_name_by_hand_survives() {
        // A hand-written list naming a tour like a temporary folder is refused before anything is touched.
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
        // And the clean-up itself never takes a listed name, whatever the list holds.
        let l = SiteList { site: 1, generator: "mythicshot".into(), title: "G".into(), tours: vec![TourEntry { slug: "old-a-1234abcd".into(), title: "x".into(), steps: 1, image: THUMB.into(), updated: T0.into() }] };
        sweep(&tmp.path().join("tours"), "a", &l);
        assert_eq!(std::fs::read_to_string(tmp.path().join("tours/old-a-1234abcd/index.html")).unwrap(), "a real tour");
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
        assert!(err.contains("tours/a/"), "{err}");
        assert!(started.elapsed() >= RETRY_FOR / 2, "it was tried again before giving up");
        assert_eq!(std::fs::read_to_string(tmp.path().join("tours/a/tour.json")).unwrap(), "{\"steps\":2}");
        drop(lock);
        assert_eq!(snapshot(tmp.path()), before);
        // Once the file is free, the same add works.
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
