//! The note an in-app update leaves for the next start, so that start can say whether the update
//! worked. No drawing here: the popup is `update_ui`'s.
//!
//! The rules, all of them on purpose:
//! - The note is written only just before msiexec is started for an in-app update
//!   ([`super::start_installer`]), and taken back when msiexec could not be started. A manual
//!   install writes none, and with the update check off no in-app update starts, so none either.
//! - It holds the version updated from and the version updated to, and nothing else: no path, no
//!   name, nothing about the person or the computer. Its age is the file's own modified time.
//! - It is beside the settings file, written whole or not at all (as the settings file is).
//! - The next start reads it and removes it at once, before anything is shown, so the popup shows
//!   once, also when the app crashes later. A note that is too large, not what the app writes, of
//!   versions that do not meet [`Version::parse`], or more than [`KEEP_FOR`] old, shows nothing,
//!   and is removed the same way. Nothing in it can stop the app from starting.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use super::Version;

/// The note's name, beside the settings file.
pub const FILE_NAME: &str = "update-pending.toml";
/// The most of the note that is read; a larger file is not the app's note.
pub const MAX_BYTES: u64 = 256;
/// How long after it was written the note still counts. An update that ends later than that is
/// not told about: the person has long seen whatever happened.
pub const KEEP_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// How far in the future a note's time may be and still count as just written: the clock may
/// have been set back a little since.
pub const FUTURE_SLACK: Duration = Duration::from_secs(10 * 60);

/// An in-app update that was started: from the running version to the version of the checked
/// installer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    pub from: Version,
    pub to: Version,
}

/// The note as it is in the file: these two fields exactly.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPending {
    from: String,
    to: String,
}

impl Pending {
    /// The file's text.
    pub fn text(&self) -> String {
        format!("from = \"{}\"\nto = \"{}\"\n", self.from, self.to)
    }

    /// Read a note: at most [`MAX_BYTES`], TOML with exactly `from` and `to`, each a version by
    /// [`Version::parse`], `to` newer than `from`. None otherwise.
    pub fn parse(bytes: &[u8]) -> Option<Pending> {
        if bytes.len() as u64 > MAX_BYTES {
            return None;
        }
        let raw: RawPending = toml::from_str(std::str::from_utf8(bytes).ok()?).ok()?;
        let (from, to) = (Version::parse(&raw.from)?, Version::parse(&raw.to)?);
        (from < to).then_some(Pending { from, to })
    }
}

/// What the start after an in-app update tells the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterUpdate {
    /// The version updated to is running.
    Updated { from: Version, to: Version },
    /// The version updated from is still running: the installer did not finish or was cancelled.
    NotFinished { from: Version },
}

/// For the log: what the start found, in words.
impl std::fmt::Display for AfterUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AfterUpdate::Updated { from, to } => write!(f, "updated from {from} to {to}"),
            AfterUpdate::NotFinished { from } => write!(f, "still {from}, the update did not finish"),
        }
    }
}

/// What the start makes of the note: `marker` is the note's bytes (None: there is none), `age`
/// how long ago it was written (None: not known), `running` the version that runs. Only a fresh
/// note the app wrote whose `to` or `from` is the running version shows anything.
pub fn decide(marker: Option<&[u8]>, age: Option<Duration>, running: Version) -> Option<AfterUpdate> {
    let pending = Pending::parse(marker?)?;
    if age? > KEEP_FOR {
        return None;
    }
    if running == pending.to {
        Some(AfterUpdate::Updated { from: pending.from, to: pending.to })
    } else if running == pending.from {
        Some(AfterUpdate::NotFinished { from: pending.from })
    } else {
        None
    }
}

/// How long before `now` a note written at `written` was written. A time a little in the future
/// is just now; one further ahead than [`FUTURE_SLACK`] is not known.
pub fn age(written: SystemTime, now: SystemTime) -> Option<Duration> {
    match now.duration_since(written) {
        Ok(age) => Some(age),
        Err(ahead) if ahead.duration() <= FUTURE_SLACK => Some(Duration::ZERO),
        Err(_) => None,
    }
}

/// Where the note is: beside the settings file (in a test build, in the test's own folder).
pub fn path() -> PathBuf {
    crate::settings::Settings::path().with_file_name(FILE_NAME)
}

/// Write the note to `path`, whole or not at all.
pub fn write(path: &Path, pending: Pending) -> std::io::Result<()> {
    crate::settings::write_whole(path, pending.text().as_bytes())
}

/// Remove the note at `path`, if a plain file (or a link) is there; a folder is not the app's.
pub fn remove(path: &Path) {
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
        let _ = std::fs::remove_file(path);
    }
}

/// At start: read the note at `path`, remove it, and say what it means for `running` at `now`.
/// Never fails: anything that goes wrong is no popup.
pub fn take(path: &Path, now: SystemTime, running: Version) -> Option<AfterUpdate> {
    let read = read_note(path);
    remove(path);
    let (bytes, written) = read?;
    decide(Some(&bytes), written.and_then(|w| age(w, now)), running)
}

/// The note's bytes (no more than [`MAX_BYTES`] and one, enough to know it is too large) and when
/// it was written. Opened without following a link (on Windows) and checked through the very
/// handle that is read, so nothing can be swapped in between: only a plain file, not too large.
fn read_note(path: &Path) -> Option<(Vec<u8>, Option<SystemTime>)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    Some((bytes, meta.modified().ok()))
}

#[cfg(test)]
mod tests;
