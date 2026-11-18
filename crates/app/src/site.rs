//! A site: a folder of plain web files holding many tours and an index page listing them.
//!
//! A folder is a site only when its own `tours.json` says so. This module decides what a tour's
//! address is and whether a folder may be used; nothing here writes to disk.

#![cfg_attr(not(test), expect(dead_code, reason = "used by the Add to site dialog, which is not built yet"))]

mod pages;

pub use pages::*;

use std::path::Path;

use crate::tour::{clean_line, tour_title};

/// The version of `tours.json` this build reads and writes.
pub const SITE_VERSION: u32 = 1;
/// The longest a slug may be.
pub const SLUG_MAX: usize = 60;
/// The longest a site title may be.
pub const SITE_TITLE_MAX: usize = 80;
/// Names this feature writes at the top of a site folder.
pub const OWN_NAMES: [&str; 5] = ["index.html", "tours.json", ".nojekyll", "assets", "tours"];
/// The picture each tour has for the index.
pub const THUMB: &str = "thumb.jpg";
/// The largest `tours.json` read. Far more than thousands of tours need; a bigger file is not ours.
const LIST_MAX_BYTES: u64 = 8 * 1_048_576;
/// Names a slug may not be: the site's own folders and page, and the Windows device names.
const RESERVED: [&str; 25] = [
    "assets", "tours", "index", "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// "Setting up raid groups" -> "setting-up-raid-groups". Never empty, never reserved.
/// ASCII letters are lowercased and digits kept; every other character becomes a hyphen, runs of
/// hyphens become one, and none is left at either end.
pub fn slug_from_title(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(SLUG_MAX).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "tour".to_string()
    } else if RESERVED.contains(&slug) {
        format!("{slug}-tour")
    } else {
        slug.to_string()
    }
}

/// A slug is 1 to [`SLUG_MAX`] characters of `a-z`, `0-9` and `-`, does not start or end with `-`,
/// has no `--` and is not a reserved name.
pub fn check_slug(slug: &str) -> Result<(), String> {
    if slug.is_empty() {
        return Err("The tour address needs a name after tours/.".to_string());
    }
    if !slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err("A tour address can use only lowercase letters a to z, digits and hyphens.".to_string());
    }
    if slug.len() > SLUG_MAX {
        return Err(format!("A tour address can be at most {SLUG_MAX} characters long."));
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return Err("A tour address cannot start or end with a hyphen or have two in a row.".to_string());
    }
    if RESERVED.contains(&slug) {
        return Err(format!("\"{slug}\" is a name the site keeps for itself; choose another address."));
    }
    Ok(())
}

/// "tours/<slug>/" -> slug. Also takes "tours/<slug>" and a bare slug.
pub fn parse_address(text: &str) -> Result<String, String> {
    let text = text.trim();
    let slug = match text.strip_prefix("tours/") {
        Some(rest) => rest.strip_suffix('/').unwrap_or(rest),
        None => text,
    };
    check_slug(slug)?;
    Ok(slug.to_string())
}

/// slug -> "tours/<slug>/"
pub fn address(slug: &str) -> String {
    format!("tours/{slug}/")
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TourEntry {
    pub slug: String,
    pub title: String,
    pub steps: u32,
    /// Always [`THUMB`].
    pub image: String,
    /// UTC, such as `2026-11-17T22:15:00Z`.
    pub updated: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SiteList {
    pub site: u32,
    pub generator: String,
    pub title: String,
    pub tours: Vec<TourEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Folder {
    New,
    Site(SiteList),
}

/// The last part of a path, for messages that should not show where the folder is.
fn shown(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "This folder".to_string())
}

/// Whether `dir` may be used, without changing anything.
///
/// `New` for an empty folder, for a folder that does not exist while its parent does, and for a folder
/// holding none of [`OWN_NAMES`]. `Site` when its `tours.json` is one this feature wrote. Everything else
/// is an error naming what is in the way. Links are never followed: a site folder, `tours.json`,
/// `tours` or `assets` that is a link is refused.
pub fn inspect(dir: &Path) -> Result<Folder, String> {
    let name = shown(dir);
    let meta = match dir.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = dir.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            return match parent.symlink_metadata() {
                Ok(m) if m.is_dir() => Ok(Folder::New),
                Ok(m) if m.file_type().is_symlink() => Err(format!("The folder that would hold {name} is a link; choose a real folder.")),
                _ => Err(format!("The folder that would hold {name} does not exist.")),
            };
        }
        Err(e) => return Err(format!("Could not open {name}: {e}")),
    };
    if meta.file_type().is_symlink() {
        return Err(format!("{name} is a link; choose the folder it points to instead."));
    }
    if !meta.is_dir() {
        return Err(format!("{name} is a file, not a folder."));
    }
    let exists = |n: &str| dir.join(n).symlink_metadata();
    let list = match exists("tours.json") {
        Ok(_) => read_list(&dir.join("tours.json"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            for own in OWN_NAMES {
                match exists(own) {
                    Ok(_) => return Err(format!("{own} is already in this folder and was not made by Mythicshot. Choose an empty folder or one that already holds a Mythicshot site.")),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("Could not check {own} in this folder: {e}")),
                }
            }
            return Ok(Folder::New);
        }
        Err(e) => return Err(format!("Could not check tours.json in this folder: {e}")),
    };
    for own in ["tours", "assets"] {
        match exists(own) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            Ok(_) => return Err(format!("{own} in this site is not a plain folder, so Mythicshot will not write into it.")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Could not check {own} in this site: {e}")),
        }
    }
    Ok(Folder::Site(list))
}

/// Read and check a site's `tours.json`. Every refusal names the file.
fn read_list(path: &Path) -> Result<SiteList, String> {
    let meta = path.symlink_metadata().map_err(|e| format!("Could not read tours.json: {e}"))?;
    if !meta.is_file() {
        return Err("tours.json in this folder is not a plain file, so this is not a site Mythicshot can add to.".to_string());
    }
    if meta.len() > LIST_MAX_BYTES {
        return Err(format!("tours.json in this folder is {} MB, far larger than Mythicshot writes, so it was not read.", meta.len() / 1_048_576));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("Could not read tours.json: {e}"))?;
    let list: SiteList = serde_json::from_str(&text).map_err(|e| format!("tours.json in this folder is damaged or was not written by Mythicshot ({e})."))?;
    if list.site != SITE_VERSION {
        return Err(format!("tours.json in this folder is for site version {}, and this Mythicshot reads version {SITE_VERSION}.", list.site));
    }
    if !list.generator.starts_with("mythicshot") {
        return Err("tours.json in this folder was not written by Mythicshot.".to_string());
    }
    let mut seen = std::collections::HashSet::new();
    for t in &list.tours {
        check_slug(&t.slug).map_err(|e| format!("tours.json lists a tour at {:?}, which is not a usable address: {e}", t.slug))?;
        if !seen.insert(t.slug.as_str()) {
            return Err(format!("tours.json lists {} more than once.", address(&t.slug)));
        }
        if t.image != THUMB {
            return Err(format!("tours.json gives {} the picture {:?}; Mythicshot writes only {THUMB}.", address(&t.slug), t.image));
        }
    }
    Ok(list)
}

/// The slug to offer: the title's slug, or with "-2", "-3"… when a tour with a different title already has it.
/// A tour with the same title is offered its own address, since adding it again is an update.
pub fn suggest_slug(title: &str, list: Option<&SiteList>) -> String {
    let title = tour_title(title);
    let base = slug_from_title(&title);
    let Some(list) = list else { return base };
    for n in 1..=list.tours.len() + 1 {
        let candidate = if n == 1 {
            base.clone()
        } else {
            let suffix = format!("-{n}");
            let stem: String = base.chars().take(SLUG_MAX - suffix.len()).collect();
            format!("{}{suffix}", stem.trim_end_matches('-'))
        };
        if check_slug(&candidate).is_err() {
            continue;
        }
        match list.tours.iter().find(|t| t.slug == candidate) {
            None => return candidate,
            Some(t) if t.title == title => return candidate,
            Some(_) => {}
        }
    }
    // Every one of n + 1 candidates cannot be taken by n tours; this is only reached if some were not
    // valid slugs, which a list from `inspect` never has.
    base
}

/// The site title as stored: cleaned to one line, capped at [`SITE_TITLE_MAX`], "Tours" when empty.
pub fn site_title(text: &str) -> String {
    let t = clean_line(text, SITE_TITLE_MAX);
    if t.is_empty() { "Tours".to_string() } else { t }
}

#[cfg(test)]
pub(crate) fn list(tours: &[(&str, &str)]) -> SiteList {
    SiteList { site: 1, generator: "mythicshot 0.1.1".into(), title: "Guides".into(), tours: tours.iter().map(|(s, t)| TourEntry { slug: s.to_string(), title: t.to_string(), steps: 3, image: "thumb.jpg".into(), updated: "2026-11-17T22:15:00Z".into() }).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_made_from_titles() {
        assert_eq!(slug_from_title("Setting up raid groups"), "setting-up-raid-groups");
        assert_eq!(slug_from_title("  Loot Master: Basics!  "), "loot-master-basics");
        assert_eq!(slug_from_title("Über Größe 2"), "ber-gr-e-2");
        assert_eq!(slug_from_title("!!!"), "tour");
        assert_eq!(slug_from_title(&"a".repeat(100)), "a".repeat(60));
        assert_eq!(slug_from_title("CON"), "con-tour");
        assert_eq!(slug_from_title("Assets"), "assets-tour");
        assert_eq!(slug_from_title(&format!("{} b", "a".repeat(59))), "a".repeat(59)); // never ends with a hyphen
        // Whatever the title, the slug passes the rules.
        for t in ["", "-", "a--b", "Tours", "index", "LPT9", "com1!", "../../etc", "a\\b/c", "\u{202E}x\u{0}", &"é".repeat(100), &"ab-".repeat(40)] {
            let s = slug_from_title(t);
            assert!(check_slug(&s).is_ok(), "{t:?} -> {s:?}");
        }
    }

    #[test]
    fn only_plain_slugs_are_accepted() {
        for ok in ["a", "setting-up-raid-groups", "v2", &"a".repeat(60)] {
            assert!(check_slug(ok).is_ok(), "{ok}");
        }
        for bad in ["", "A", "a b", "a_b", "-a", "a-", "a--b", "a/b", "a\\b", "..", ".a", "a.", "é", &"a".repeat(61), "assets", "tours", "index", "con", "nul", "com1", "lpt9"] {
            assert!(check_slug(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn addresses_stay_inside_the_site() {
        assert_eq!(parse_address("tours/setting-up-raid-groups/").unwrap(), "setting-up-raid-groups");
        assert_eq!(parse_address("tours/a-b").unwrap(), "a-b");
        assert_eq!(parse_address(" a-b ").unwrap(), "a-b");
        assert_eq!(address("a-b"), "tours/a-b/");
        for bad in ["", "tours/", "tours/../x/", "tours/a/b/", "/tours/a/", "C:/x", "tours\\a", "tours/A/", "tours/assets/", "tours/con/", "docs/a/", "tours/a/../b", "tours//", "tours/a//", "tours", "./a", "a/", "%2e%2e", "tours/a\u{0}"] {
            assert!(parse_address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_second_tour_with_the_same_slug_is_offered_another_address() {
        assert_eq!(suggest_slug("Raid groups", None), "raid-groups");
        let l = list(&[("raid-groups", "Raid groups"), ("raid-groups-2", "Raid Groups!")]);
        assert_eq!(suggest_slug("Raid groups", Some(&l)), "raid-groups"); // same title: an update
        assert_eq!(suggest_slug("Raid: groups", Some(&l)), "raid-groups-3"); // different title, same slug
        assert_eq!(suggest_slug("Raid Groups!", Some(&l)), "raid-groups-2"); // its own address
        // A long title keeps the suffix inside the length limit.
        let long = "a".repeat(80);
        let l = list(&[(&"a".repeat(60), "Something else")]);
        let s = suggest_slug(&long, Some(&l));
        assert_eq!(s, format!("{}-2", "a".repeat(58)));
        assert!(check_slug(&s).is_ok());
        // An empty title is offered the address of the title the tour will carry.
        assert_eq!(suggest_slug(" ", None), "untitled-tour");
    }

    #[test]
    fn a_folder_is_new_a_site_or_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(inspect(tmp.path()).unwrap(), Folder::New);
        assert_eq!(inspect(&tmp.path().join("docs")).unwrap(), Folder::New);
        assert!(inspect(&tmp.path().join("no").join("docs")).is_err());
        let docs = tmp.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        std::fs::write(docs.join("README.md"), "x").unwrap();
        std::fs::write(docs.join("CNAME"), "x").unwrap();
        assert_eq!(inspect(&docs).unwrap(), Folder::New);
        for name in OWN_NAMES.iter().filter(|n| **n != "tours.json") {
            let d = tmp.path().join(format!("has-{}", name.trim_start_matches('.')));
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join(name), "x").unwrap();
            let err = inspect(&d).unwrap_err();
            assert!(err.contains(name), "{err}");
        }
        let file = tmp.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(inspect(&file).is_err());
        // A folder of that name counts as much as a file.
        let d = tmp.path().join("has-tours-folder");
        std::fs::create_dir_all(d.join("tours")).unwrap();
        assert!(inspect(&d).unwrap_err().contains("tours"));
        // Nothing was written by any of these.
        assert_eq!(std::fs::read_dir(&docs).unwrap().count(), 2);
        assert!(!tmp.path().join("docs").join("tours.json").exists());
    }

    #[test]
    fn a_damaged_tours_json_is_refused_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let good = serde_json::to_string(&list(&[("a", "A")])).unwrap();
        std::fs::write(tmp.path().join("tours.json"), &good).unwrap();
        assert_eq!(inspect(tmp.path()).unwrap(), Folder::Site(list(&[("a", "A")])));
        let not_thumb = good.replace("thumb.jpg", "../../secret.jpg");
        for bad in [
            "{",
            "[]",
            "",
            "null",
            r#"{"site":2,"generator":"mythicshot 1","title":"t","tours":[]}"#,
            r#"{"site":1,"generator":"other","title":"t","tours":[]}"#,
            r#"{"site":1,"generator":"mythicshot 1","title":"t","tours":[{"slug":"a"}]}"#,
            r#"{"site":1,"generator":"mythicshot 1","title":"t","tours":[{"slug":"a","title":"A","steps":-1,"image":"thumb.jpg","updated":""}]}"#,
            r#"{"site":1,"generator":"mythicshot 1","title":"t","tours":{}}"#,
            &good.replace("\"a\"", "\"../a\""),
            &good.replace("\"a\"", "\"assets\""),
            &not_thumb,
            &serde_json::to_string(&list(&[("a", "A"), ("a", "B")])).unwrap(),
        ] {
            std::fs::write(tmp.path().join("tours.json"), bad).unwrap();
            let err = inspect(tmp.path()).unwrap_err();
            assert!(err.contains("tours.json"), "{bad}: {err}");
        }
        // Bytes that are not text, and a file far too large to be ours.
        std::fs::write(tmp.path().join("tours.json"), [0xff, 0xfe, 0x00, 0x7b]).unwrap();
        assert!(inspect(tmp.path()).unwrap_err().contains("tours.json"));
        let f = std::fs::File::create(tmp.path().join("tours.json")).unwrap();
        f.set_len(9 * 1_048_576).unwrap();
        drop(f);
        assert!(inspect(tmp.path()).unwrap_err().contains("tours.json"));
        // A folder named tours.json is not a site either.
        let d = tmp.path().join("odd");
        std::fs::create_dir_all(d.join("tours.json")).unwrap();
        assert!(inspect(&d).unwrap_err().contains("tours.json"));
        // A site whose tours is a file is refused.
        let d = tmp.path().join("site");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(d.join("tours.json"), &good).unwrap();
        std::fs::write(d.join("tours"), "x").unwrap();
        assert!(inspect(&d).unwrap_err().contains("tours"));
    }

    #[test]
    fn links_are_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&real, &link);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&real, &link);
        if made.is_err() {
            // Creating links needs a privilege Windows does not give every account.
            return;
        }
        assert!(inspect(&link).unwrap_err().contains("link"));
        let site = tmp.path().join("site");
        std::fs::create_dir(&site).unwrap();
        std::fs::write(site.join("tours.json"), serde_json::to_string(&list(&[])).unwrap()).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, site.join("assets")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, site.join("assets")).unwrap();
        assert!(inspect(&site).unwrap_err().contains("assets"));
    }

    #[test]
    fn site_titles_are_one_clean_line() {
        assert_eq!(site_title("  Raid tool guides \n"), "Raid tool guides");
        assert_eq!(site_title(""), "Tours");
        assert_eq!(site_title(" \u{200B}\t"), "Tours");
        assert_eq!(site_title("a\nb\u{202E}c"), "a bc");
        assert_eq!(site_title(&"x".repeat(200)).chars().count(), 80);
    }
}
