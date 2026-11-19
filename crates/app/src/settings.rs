//! Settings file: `settings.toml` in the OS config folder
//! (`%APPDATA%\Mythicshot\config` on Windows).

use std::collections::BTreeMap;
use std::path::PathBuf;

use mythicshot_destinations::{Context, DEFAULT_PATTERN, DestId, ImageFormat};
use mythicshot_editor::overlays::{KeyStyle, OverlayStyle};
use serde::{Deserialize, Serialize};

use crate::hotkeys::{Action, EditorAction};

/// Bumped when default editor keys change in a way old files need moving.
const EDITOR_KEYS_VERSION: u32 = 1;

/// What a left-click on the tray icon does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayLeftClick {
    /// Open the main window on its Home page.
    #[default]
    OpenApp,
    /// Start a region capture straight away, as every version before the main window did.
    CaptureRegion,
}

impl TrayLeftClick {
    pub const ALL: [TrayLeftClick; 2] = [TrayLeftClick::OpenApp, TrayLeftClick::CaptureRegion];

    /// The choice as the Preferences page words it.
    pub fn label(self) -> &'static str {
        match self {
            TrayLeftClick::OpenApp => "Open Mythicshot",
            TrayLeftClick::CaptureRegion => "Capture a region",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub output_dir: PathBuf,
    /// Where each capture goes, in order. `Picker` shows a menu instead.
    /// `None` only while loading a file from before destinations existed.
    pub destinations: Option<Vec<DestId>>,
    /// File name pattern for saved captures, see `mythicshot_destinations::expand_pattern`.
    pub filename_pattern: String,
    pub image_format: ImageFormat,
    pub jpeg_quality: u8,
    pub magnifier: bool,
    /// Take Print Screen shortcuts before other apps (OneDrive, Snipping Tool
    /// and the like) on Windows, instead of asking Windows for them.
    pub take_print_screen: bool,
    /// Recording: frames per second and the countdown before it starts.
    pub record_fps: u32,
    pub countdown_secs: u32,
    pub gif_quality: u8,
    /// Log key presses while recording, for the key overlay. Off by default:
    /// the log could include anything typed, passwords too.
    pub record_keys: bool,
    /// Overlays new recordings start with; the GIF editor can change them.
    pub overlays: OverlayStyle,
    /// Last captured region in desktop pixels: x, y, width, height.
    pub last_region: Option<[i32; 4]>,
    /// Action → shortcut in `global-hotkey` syntax, for example `shift+PrintScreen`.
    /// An empty string means the action has no shortcut.
    pub hotkeys: BTreeMap<Action, String>,
    /// Editor action → shortcut, same syntax as `hotkeys`.
    pub editor_keys: BTreeMap<EditorAction, String>,
    /// What the editor remembers between captures.
    pub editor: EditorPrefs,
    /// Which default editor keys this file has seen; see [`Settings::upgrade`].
    /// Files from before it existed read as 0, not as today's defaults.
    #[serde(default)]
    pub editor_keys_version: u32,
    /// Tour platform address for "Publish tour", empty until set. The API token is never
    /// kept here, only in the OS credential store (see `publish`).
    pub publish_server: String,
    /// What a left-click on the tray icon does. Files from before the main window have no such key
    /// and read as `OpenApp`.
    pub tray_left_click: TrayLeftClick,
    /// The site address the saved API credential was saved for; it is never sent to any other
    /// address. An empty string, or no key, means it is tied to none.
    pub publish_saved_for: Option<String>,
    /// The one-time adoption for a file from before `publish_saved_for` existed has run: the
    /// address configured at that start, if any, was recorded there. Set at the first start of
    /// a version that knows it, whatever the credential store said, and never run again, so an
    /// address typed later is never adopted without the user saying so.
    pub publish_binding_settled: bool,
    /// Keep the recording and its edit state on this computer when a tour is uploaded, so the
    /// tour can be opened and updated later. On by default, also for a file without the key.
    pub keep_published_recordings: bool,
    /// Store kept recordings as near-lossless video in the background when ffmpeg is on this
    /// computer, to save space. On by default, also for a file without the key. Turning it off
    /// converts nothing back.
    pub store_recordings_as_video: bool,
    /// Warn when the saved recordings together take more than this many GB; 0 means no warning.
    /// Nothing is ever removed because of it. A file may hold 0 to `LIMIT_MAX_GB`; anything else
    /// there costs only this value.
    #[serde(deserialize_with = "warn_gb")]
    pub saved_recordings_warn_gb: u32,
    /// Tours whose delete removed the tour but not yet all of its files. Kept until the site
    /// says nothing is left, so the delete can be finished after a restart.
    pub unfinished_deletes: Vec<UnfinishedDelete>,
    /// What went wrong reading the settings file at start-up, if anything. Never written to the file.
    #[serde(skip)]
    pub load_problem: Option<LoadProblem>,

    // Phase 1–2 options, read once to fill `destinations`, never written.
    #[serde(skip_serializing)]
    open_editor: Option<bool>,
    #[serde(skip_serializing)]
    save_to_file: Option<bool>,
    #[serde(skip_serializing)]
    copy_to_clipboard: Option<bool>,
}

/// The size past which saved recordings are warned about until the user sets another, in GB.
const DEFAULT_WARN_GB: u32 = 5;

/// The warning limit as the settings file holds it: a whole number no larger than the field in
/// Preferences can hold. Anything else is refused, which costs only this value (see [`Settings::parse`]).
fn warn_gb<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let gb = u32::deserialize(d)?;
    if gb > crate::project::LIMIT_MAX_GB {
        return Err(serde::de::Error::custom("the limit is larger than the largest that can be set"));
    }
    Ok(gb)
}

/// A delete to finish: which site, which tour, and the title to show for it. No secret is in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnfinishedDelete {
    pub server: String,
    pub tour_id: String,
    pub title: String,
}

/// The settings file was there at start-up but could not be used as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadProblem {
    /// The settings file.
    pub path: PathBuf,
    /// The copy of the file as it was, when one could be made.
    pub backup: Option<PathBuf>,
    /// Some settings were kept; the others read as their defaults.
    pub partly_read: bool,
}

impl LoadProblem {
    /// Without a copy of the file as it was, nothing is saved over it this session.
    pub fn keeps_file_untouched(&self) -> bool {
        self.backup.is_none()
    }

    /// What the Preferences page says about it.
    pub fn sentence(&self) -> String {
        match (&self.backup, self.partly_read) {
            (None, _) => format!("The settings file could not be read, so changes will not be saved. The file is {}.", self.path.display()),
            (Some(copy), true) => format!("Some settings could not be read and are back to their defaults. A copy of the file as it was is {}.", copy.display()),
            (Some(copy), false) => format!("The settings file could not be read, so the defaults are in use. A copy of the file as it was is {}.", copy.display()),
        }
    }
}

/// How many copies of unreadable settings files are kept beside the settings file.
const BACKUPS_KEPT: usize = 5;
const BACKUP_PREFIX: &str = "settings.unreadable-";

/// The time stamp and counter in a backup's name, `settings.unreadable-20261118-093000.toml` or
/// `settings.unreadable-20261118-093000-2.toml`; `None` for any other file.
fn backup_stamp(name: &str) -> Option<(String, u32)> {
    let rest = name.strip_prefix(BACKUP_PREFIX)?.strip_suffix(".toml")?;
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = rest.split('-');
    let (date, time) = (parts.next()?, parts.next()?);
    let counter = match parts.next() {
        None => 1,
        Some(c) if !c.is_empty() && c.len() <= 4 && c.bytes().all(|b| b.is_ascii_digit()) => c.parse().ok()?,
        Some(_) => return None,
    };
    (parts.next().is_none() && digits(date, 8) && digits(time, 6)).then(|| (format!("{date}-{time}"), counter))
}

/// Keep a copy of the settings file as it was, beside it, under a name with the time in it, and
/// remove all but the newest [`BACKUPS_KEPT`] such copies. Only files with exactly that kind of
/// name are ever removed.
fn back_up(path: &std::path::Path, bytes: &[u8], now: chrono::NaiveDateTime) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();
    let mut made = None;
    for n in 1..=9999u32 {
        let name = if n == 1 { format!("{BACKUP_PREFIX}{stamp}.toml") } else { format!("{BACKUP_PREFIX}{stamp}-{n}.toml") };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&name)) {
            Ok(mut f) => {
                f.write_all(bytes)?;
                f.sync_all()?;
                made = Some(dir.join(name));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    let made = made.ok_or_else(|| std::io::Error::other("too many copies with the same time"))?;
    let mut copies: Vec<((String, u32), PathBuf)> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| backup_stamp(&e.file_name().to_string_lossy()).map(|stamp| (stamp, e.path())))
        .collect();
    copies.sort();
    let extra = copies.len().saturating_sub(BACKUPS_KEPT);
    for (_, old) in copies.into_iter().take(extra) {
        let _ = std::fs::remove_file(old);
    }
    Ok(made)
}

/// One step from the settings file's top down to a value in it.
enum Step {
    Key(String),
    /// An entry of a list.
    Item,
}

/// Whether the settings would load with only `value` in the file, at the place `steps` leads to.
fn fits(steps: &[Step], value: &toml::Value) -> bool {
    let wrapped = steps.iter().rev().fold(value.clone(), |inner, step| match step {
        Step::Key(k) => toml::Value::Table(toml::Table::from_iter([(k.clone(), inner)])),
        Step::Item => toml::Value::Array(vec![inner]),
    });
    wrapped.try_into::<Settings>().is_ok()
}

/// `value` with everything this version cannot read taken out, however deep: a bad entry of a
/// section or of a list goes, its neighbours stay. `None` when nothing of it can be kept.
fn salvage(steps: &mut Vec<Step>, value: toml::Value) -> Option<toml::Value> {
    if fits(steps, &value) {
        return Some(value);
    }
    let kept = match value {
        toml::Value::Table(section) => toml::Value::Table(
            section
                .into_iter()
                .filter_map(|(key, inner)| {
                    steps.push(Step::Key(key.clone()));
                    let kept = salvage(steps, inner);
                    steps.pop();
                    kept.map(|v| (key, v))
                })
                .collect(),
        ),
        toml::Value::Array(list) => toml::Value::Array(
            list.into_iter()
                .filter_map(|inner| {
                    steps.push(Step::Item);
                    let kept = salvage(steps, inner);
                    steps.pop();
                    kept
                })
                .collect(),
        ),
        _ => return None,
    };
    fits(steps, &kept).then_some(kept)
}

/// A drawing style as saved in the settings file.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedStyle {
    pub stroke: [u8; 4],
    pub fill: Option<[u8; 4]>,
    pub width: f32,
    pub font_size: f32,
    pub shadow: bool,
    /// For the obfuscate tool: blur instead of pixelate.
    pub blur: bool,
}

impl Default for SavedStyle {
    fn default() -> Self {
        Self { stroke: [229, 72, 77, 255], fill: None, width: 3.0, font_size: 20.0, shadow: true, blur: false }
    }
}

/// The editor's remembered choices: a style per tool (like Greenshot), the
/// last tool used, and the GIF editor's export settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorPrefs {
    pub tool: String,
    /// Tool name → its style, for example `text` → white 32 pt.
    pub styles: BTreeMap<String, SavedStyle>,
    pub export_format: String,
    pub export_scale: u32,
    pub export_quality: u8,
    pub export_loop: bool,
    pub export_fps: u32,
    /// Spotlight darkening strength in percent, for the next spotlight and the next recording.
    pub spotlight_dim: u8,
    /// The last tour's theme and zoom, which the next recording's tour starts with.
    pub tour_theme: SavedTheme,
    /// The Tour view pulses the hotspot as the tour does; off draws it still.
    pub tour_animate_hotspot: bool,
    /// "Add to site": the site folder and the site's title used last, empty until a tour is added.
    pub site_folder: String,
    pub site_title: String,
}

/// A tour's look as saved in the settings file: the Tour settings a new recording starts with.
/// Cover, buttons, end card and autoplay are never remembered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTheme {
    /// "dark", "light", "compact" or "auto".
    pub card_style: String,
    pub accent: String,
    /// "pulse", "dot" or "none".
    pub hotspot: String,
    pub progress_bar: bool,
    /// "auto" or "#rrggbb".
    pub backdrop: String,
    pub spotlight_soft: bool,
    pub zoom_enabled: bool,
    pub zoom_percent: u16,
}

impl Default for SavedTheme {
    fn default() -> Self {
        Self { card_style: "dark".into(), accent: crate::tour::DEFAULT_ACCENT.into(), hotspot: "pulse".into(), progress_bar: true, backdrop: "auto".into(), spotlight_soft: true, zoom_enabled: true, zoom_percent: 118 }
    }
}

impl Default for EditorPrefs {
    fn default() -> Self {
        Self { tool: "rectangle".into(), styles: BTreeMap::new(), export_format: "gif".into(), export_scale: 100, export_quality: 90, export_loop: true, export_fps: 30, spotlight_dim: mythicshot_editor::DEFAULT_DIM, tour_theme: SavedTheme::default(), tour_animate_hotspot: true, site_folder: String::new(), site_title: String::new() }
    }
}

impl Default for Settings {
    fn default() -> Self {
        let pictures = directories::UserDirs::new()
            .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            output_dir: pictures.join("Mythicshot"),
            destinations: Some(vec![DestId::Editor]),
            filename_pattern: DEFAULT_PATTERN.to_string(),
            image_format: ImageFormat::Png,
            jpeg_quality: 90,
            magnifier: true,
            take_print_screen: true,
            record_fps: 15,
            countdown_secs: 3,
            gif_quality: 90,
            record_keys: false,
            overlays: OverlayStyle { keys: KeyStyle::Chips, ..OverlayStyle::default() },
            last_region: None,
            hotkeys: Action::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect(),
            editor_keys: EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect(),
            editor: EditorPrefs::default(),
            editor_keys_version: EDITOR_KEYS_VERSION,
            publish_server: String::new(),
            tray_left_click: TrayLeftClick::default(),
            publish_saved_for: None,
            publish_binding_settled: false,
            keep_published_recordings: true,
            store_recordings_as_video: true,
            saved_recordings_warn_gb: DEFAULT_WARN_GB,
            unfinished_deletes: Vec::new(),
            load_problem: None,
            open_editor: None,
            save_to_file: None,
            copy_to_clipboard: None,
        }
    }
}

/// Where the settings file's new text is written before it takes the file's place: beside it,
/// so the rename stays on one drive.
fn temp_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Every write of the settings file goes through here, with [`writing`] held by the caller.
/// The file is never written in place: the new text goes to a temporary file beside it, is
/// flushed to the disk, and then takes the file's place in one rename. A crash or a failure
/// at any point leaves the settings file whole, as it was or as it should be.
fn write_file(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let write = |tmp: &std::path::Path| {
        // A temporary file left by a crash is written over from its start.
        let mut f = std::fs::File::create(tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()
    };
    replace_with(path, write, |from, to| std::fs::rename(from, to), RENAME_TRIES, || std::thread::sleep(RENAME_WAIT))
}

/// An error of the kind another program causes for a moment by having the file open: no
/// permission (Windows error 5) or a sharing violation (32).
fn is_busy(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32))
}

/// How often the rename is tried, and how long is waited between two tries: half a second in all.
const RENAME_TRIES: u32 = 5;
const RENAME_WAIT: std::time::Duration = std::time::Duration::from_millis(120);

/// How long after a save that failed the app tries again by itself.
pub const SAVE_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// What the app remembers of a save of the settings file that failed: the change is still
/// only in memory, and is written again later, and when the app closes.
#[derive(Debug, Default)]
pub struct SaveRetry {
    again_at: Option<std::time::Instant>,
}

impl SaveRetry {
    /// A save ended, well or not.
    pub fn record(&mut self, result: &std::io::Result<()>, now: std::time::Instant) {
        self.again_at = result.is_err().then(|| now + SAVE_RETRY_AFTER);
    }

    /// The settings file does not hold what the app has.
    pub fn pending(&self) -> bool {
        self.again_at.is_some()
    }

    /// Whether a save nobody asked for may run now: always, except shortly after one that failed.
    pub fn allows(&self, now: std::time::Instant) -> bool {
        self.again_at.is_none_or(|at| now >= at)
    }
}

/// The steps of [`write_file`]: `write` fills the temporary file, `rename` moves it over the
/// file at `path`, up to `tries` times with a `wait` between them while another program is in
/// the way. When either fails the file at `path` has not been touched, the temporary
/// file is removed if it can be, and the error is the caller's to report.
fn replace_with(
    path: &std::path::Path,
    write: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    mut rename: impl FnMut(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
    tries: u32,
    mut wait: impl FnMut(),
) -> std::io::Result<()> {
    let tmp = temp_path(path);
    let result = write(&tmp).and_then(|()| {
        // Another program (a scanner, a backup, a sync) may have either file open for a moment.
        let mut tried = 1;
        loop {
            match rename(&tmp, path) {
                Err(e) if is_busy(&e) && tried < tries => {
                    tried += 1;
                    wait();
                }
                other => return other,
            }
        }
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// One writer of the settings file at a time: the app's own save, and a background job's note.
fn writing() -> std::sync::MutexGuard<'static, ()> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    WRITING.lock().unwrap_or_else(|e| e.into_inner())
}

impl Settings {
    pub fn path() -> PathBuf {
        // Debug builds can use a throwaway config folder for tests.
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("MYTHICSHOT_CONFIG_DIR") {
            return PathBuf::from(dir).join("settings.toml");
        }
        directories::ProjectDirs::from("", "", "Mythicshot")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("settings.toml")
    }

    pub fn load() -> Self {
        let s = Self::load_with(&Self::path(), |p| std::fs::read(p), |p, bytes| back_up(p, bytes, chrono::Local::now().naive_local()));
        if let Some(problem) = &s.load_problem {
            eprintln!("{}", problem.sentence());
        }
        s
    }

    /// Read the settings file at `path`.
    ///
    /// A file that is not there is a first start: the defaults, saved as usual. A file that is
    /// there but cannot be used as it is (unreadable, not text, not TOML, or holding values this
    /// version does not understand) is first copied aside by `backup`, because the next change
    /// saves over it. When the file could not be read, or the copy could not be made, the app
    /// runs on what it has for this session and never saves over the file (see [`Settings::save`]).
    fn load_with(path: &std::path::Path, read: impl FnOnce(&std::path::Path) -> std::io::Result<Vec<u8>>, backup: impl FnOnce(&std::path::Path, &[u8]) -> std::io::Result<PathBuf>) -> Self {
        let problem = |backup: Option<PathBuf>, partly_read: bool| Some(LoadProblem { path: path.to_path_buf(), backup, partly_read });
        let bytes = match read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default().upgrade(),
            // Locked, no permission, a drive error: what is in it is unknown, so it is left alone.
            Err(_) => return Settings { load_problem: problem(None, false), ..Settings::default().upgrade() },
        };
        let strict = std::str::from_utf8(&bytes).ok().and_then(|text| toml::from_str::<Settings>(text).ok());
        if let Some(s) = strict {
            return s.upgrade();
        }
        let partly = std::str::from_utf8(&bytes).ok().and_then(Self::parse);
        let copy = backup(path, &bytes).ok();
        let partly_read = partly.is_some();
        Settings { load_problem: problem(copy, partly_read), ..partly.unwrap_or_default().upgrade() }
    }

    /// Settings from the file's text. A value this version does not understand (a mistyped word, a
    /// choice or an action from a newer version, a number where text belongs) costs only that
    /// one value, which reads as its default; everything else is kept, in sections and lists too.
    /// Without this, one such value made the whole file read as the defaults, and the next save
    /// wrote them over it. `None` only when the text is not TOML.
    fn parse(text: &str) -> Option<Settings> {
        if let Ok(s) = toml::from_str::<Settings>(text) {
            return Some(s);
        }
        let table: toml::Table = toml::from_str(text).ok()?;
        salvage(&mut Vec::new(), toml::Value::Table(table))?.try_into().ok()
    }

    /// Bring a file written by an older version up to date.
    fn upgrade(self) -> Self {
        let mut s = self;
        // Before recording existed, Shift+PrtSc was the default for last region.
        // Files from then get it moved to Record region, the new default.
        if !s.hotkeys.contains_key(&Action::RecordRegion) && s.binding(Action::CaptureLastRegion).eq_ignore_ascii_case("shift+PrintScreen") {
            s.hotkeys.insert(Action::CaptureLastRegion, String::new());
        }
        if s.destinations.is_none() {
            s.destinations = Some(s.migrate_destinations());
        }
        for a in Action::ALL {
            s.hotkeys.entry(*a).or_insert_with(|| a.default_binding().to_string());
        }
        // Version 1 gave F to the asterisk and C to the cone: the pen moved to W
        // and crop to K. Keys still on the old default move; rebound ones stay.
        if s.editor_keys_version < 1 {
            for (a, old) in [(EditorAction::Freehand, "KeyF"), (EditorAction::Crop, "KeyC")] {
                if s.editor_keys.get(&a).is_some_and(|b| b == old) {
                    s.editor_keys.insert(a, a.default_binding().to_string());
                }
            }
        }
        s.editor_keys_version = EDITOR_KEYS_VERSION;
        for a in EditorAction::ALL {
            s.editor_keys.entry(*a).or_insert_with(|| a.default_binding().to_string());
        }
        s
    }

    /// Destinations from the Phase 1–2 options: the editor if it was on,
    /// otherwise save and/or copy.
    fn migrate_destinations(&self) -> Vec<DestId> {
        if self.open_editor.unwrap_or(true) {
            return vec![DestId::Editor];
        }
        let mut d = Vec::new();
        if self.save_to_file.unwrap_or(true) {
            d.push(DestId::File);
        }
        if self.copy_to_clipboard.unwrap_or(true) {
            d.push(DestId::Clipboard);
        }
        d
    }

    /// Write the settings file. An error means the file is as it was and the change is still
    /// only in memory: the caller keeps it to be saved again.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::path())
    }

    /// How a job writes its note to the settings file at once; `None` when the file could not
    /// be read at start-up and is never written over, so that no new delete is sent.
    pub fn note_saver(&self) -> Option<fn(&[UnfinishedDelete]) -> bool> {
        let untouched = self.load_problem.as_ref().is_some_and(LoadProblem::keeps_file_untouched);
        (!untouched).then_some(Self::save_unfinished_deletes as fn(&[UnfinishedDelete]) -> bool)
    }

    /// Write `list` as the file's unfinished deletes, now, from whatever thread asks, leaving
    /// every other value in the file as it is. For a note that must be on disk before a
    /// request goes out. False when it could not be written.
    pub fn save_unfinished_deletes(list: &[UnfinishedDelete]) -> bool {
        Self::patch_unfinished_deletes(&Self::path(), list)
    }

    fn patch_unfinished_deletes(path: &std::path::Path, list: &[UnfinishedDelete]) -> bool {
        let _writing = writing();
        let mut table: toml::Table = match std::fs::read(path) {
            // A file that is not TOML is not written over here; the app's own save decides about it.
            Ok(bytes) => match std::str::from_utf8(&bytes).ok().and_then(|text| toml::from_str(text).ok()) {
                Some(table) => table,
                None => return false,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(_) => return false,
        };
        let Ok(value) = toml::Value::try_from(list.to_vec()) else { return false };
        table.insert("unfinished_deletes".to_string(), value);
        toml::to_string_pretty(&table).is_ok_and(|text| write_file(path, &text).is_ok())
    }

    /// Write the settings to `path`, unless the file there could not be read at start-up and no
    /// copy of it could be made: then it is never written over.
    fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        // Nothing is to be saved this session, so nothing is left unsaved either.
        if self.load_problem.as_ref().is_some_and(LoadProblem::keeps_file_untouched) {
            return Ok(());
        }
        let _writing = writing();
        let text = toml::to_string_pretty(self).map_err(|e| {
            eprintln!("could not encode settings: {e}");
            std::io::Error::other(e)
        })?;
        write_file(path, &text).inspect_err(|e| eprintln!("could not save settings: {e}"))
    }

    pub fn binding(&self, action: Action) -> &str {
        self.hotkeys.get(&action).map(String::as_str).unwrap_or("")
    }

    pub fn editor_binding(&self, action: EditorAction) -> &str {
        self.editor_keys.get(&action).map(String::as_str).unwrap_or("")
    }

    pub fn destinations(&self) -> &[DestId] {
        self.destinations.as_deref().unwrap_or(&[DestId::Editor])
    }

    pub fn has_destination(&self, d: DestId) -> bool {
        self.destinations().contains(&d)
    }

    /// Turn a destination on or off, keeping the menu order.
    pub fn toggle_destination(&mut self, d: DestId) {
        let mut list: Vec<DestId> = self.destinations().to_vec();
        if list.contains(&d) {
            list.retain(|x| *x != d);
        } else {
            list.push(d);
            list.sort_by_key(|x| DestId::ALL.iter().position(|a| a == x));
        }
        self.destinations = Some(list);
    }

    /// Take over what the Preferences and Workspace pages changed in their copy, `edited`:
    /// every field one of those pages edits directly, and no other. Shortcuts go through
    /// `SetBinding`, the Print Screen switch has its own action, and the rest is the app's own
    /// and may be newer here than in the window's copy.
    pub fn take_edited(&mut self, edited: &Settings) {
        // Every field is named, so a new one does not compile until it is decided here
        // whether a page edits it.
        let Settings {
            output_dir,
            destinations,
            filename_pattern,
            image_format,
            jpeg_quality,
            magnifier,
            record_fps,
            countdown_secs,
            gif_quality,
            record_keys,
            overlays,
            publish_server,
            tray_left_click,
            keep_published_recordings,
            store_recordings_as_video,
            saved_recordings_warn_gb,
            // Not edited by a page.
            take_print_screen: _,
            last_region: _,
            hotkeys: _,
            editor_keys: _,
            editor: _,
            editor_keys_version: _,
            publish_saved_for: _,
            publish_binding_settled: _,
            unfinished_deletes: _,
            load_problem: _,
            open_editor: _,
            save_to_file: _,
            copy_to_clipboard: _,
        } = edited;
        self.output_dir = output_dir.clone();
        self.destinations = destinations.clone();
        self.filename_pattern = filename_pattern.clone();
        self.image_format = *image_format;
        self.jpeg_quality = *jpeg_quality;
        self.magnifier = *magnifier;
        self.record_fps = *record_fps;
        self.countdown_secs = *countdown_secs;
        self.gif_quality = *gif_quality;
        self.record_keys = *record_keys;
        self.overlays = *overlays;
        self.publish_server = publish_server.clone();
        self.tray_left_click = *tray_left_click;
        self.keep_published_recordings = *keep_published_recordings;
        self.store_recordings_as_video = *store_recordings_as_video;
        self.saved_recordings_warn_gb = *saved_recordings_warn_gb;
    }

    pub fn dest_context(&self, title: &str) -> Context {
        Context {
            output_dir: self.output_dir.clone(),
            pattern: self.filename_pattern.clone(),
            format: self.image_format,
            jpeg_quality: self.jpeg_quality,
            title: title.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_the_pages_edit_is_carried_over() {
        use mythicshot_editor::overlays::ClickStyle;
        let mut mine = Settings::default();
        mine.hotkeys.insert(Action::ALL[0], "control+KeyQ".to_string());
        let before = mine.clone();
        // The window's copy, with every field a page edits changed.
        let mut edited = mine.clone();
        edited.output_dir = PathBuf::from("D:/Shots");
        edited.toggle_destination(DestId::Clipboard);
        edited.filename_pattern = "shot ${hh}".to_string();
        edited.image_format = ImageFormat::Jpeg;
        edited.jpeg_quality = 41;
        edited.magnifier = !mine.magnifier;
        edited.record_fps = 24;
        edited.countdown_secs = 10;
        edited.gif_quality = 51;
        edited.record_keys = !mine.record_keys;
        edited.overlays = OverlayStyle { keys: KeyStyle::Keyboard, clicks: ClickStyle::Both, halo: true, pointer: false };
        edited.publish_server = "https://tours.example.com".to_string();
        edited.tray_left_click = TrayLeftClick::CaptureRegion;
        edited.keep_published_recordings = false;
        edited.store_recordings_as_video = false;
        edited.saved_recordings_warn_gb = 12;
        assert!(mine.keep_published_recordings && mine.store_recordings_as_video && mine.saved_recordings_warn_gb == 5 && mine.overlays != edited.overlays);
        assert!(mine.image_format != edited.image_format && mine.tray_left_click != edited.tray_left_click && mine.destinations != edited.destinations);
        // And what no page edits changed too: a window's copy can be a frame behind the app's.
        edited.hotkeys.clear();
        edited.editor_keys.clear();
        edited.take_print_screen = !mine.take_print_screen;
        edited.last_region = Some([1, 2, 3, 4]);
        edited.editor_keys_version = 99;
        edited.publish_saved_for = Some("https://elsewhere.example.com".to_string());
        edited.publish_binding_settled = !mine.publish_binding_settled;

        mine.take_edited(&edited);
        assert_eq!(mine.output_dir, PathBuf::from("D:/Shots"));
        assert!(mine.destinations == edited.destinations && mine.has_destination(DestId::Clipboard) != before.has_destination(DestId::Clipboard));
        assert_eq!(mine.filename_pattern, "shot ${hh}");
        assert!(mine.image_format == ImageFormat::Jpeg && mine.tray_left_click == TrayLeftClick::CaptureRegion);
        assert_eq!((mine.jpeg_quality, mine.record_fps, mine.countdown_secs, mine.gif_quality), (41, 24, 10, 51));
        assert_eq!((mine.magnifier, mine.record_keys), (!before.magnifier, !before.record_keys));
        assert!(mine.overlays == edited.overlays);
        assert_eq!(mine.publish_server, "https://tours.example.com");
        // The three of the saved-recordings section.
        assert!(!mine.keep_published_recordings, "the keep switch is carried over");
        assert!(!mine.store_recordings_as_video, "the video switch is carried over");
        assert_eq!(mine.saved_recordings_warn_gb, 12, "the limit is carried over");
        // Shortcuts go their own way, and the rest is the app's own.
        assert_eq!((&mine.hotkeys, &mine.editor_keys), (&before.hotkeys, &before.editor_keys));
        assert_eq!(mine.binding(Action::ALL[0]), "control+KeyQ");
        assert_eq!((mine.take_print_screen, mine.last_region, mine.editor_keys_version), (before.take_print_screen, before.last_region, before.editor_keys_version));
        assert_eq!((&mine.publish_saved_for, mine.publish_binding_settled), (&before.publish_saved_for, before.publish_binding_settled));
        // Taken twice is taken once, and an unchanged copy changes nothing.
        let once = toml::to_string(&mine).unwrap();
        mine.take_edited(&edited);
        assert_eq!(toml::to_string(&mine).unwrap(), once);
        let mut same = before.clone();
        same.take_edited(&before);
        assert_eq!(toml::to_string(&same).unwrap(), toml::to_string(&before).unwrap());
    }

    #[test]
    fn pen_and_crop_move_off_f_and_c_unless_rebound() {
        // An older file: pen and crop on their old defaults, select rebound.
        let old: Settings = toml::from_str("[editor_keys]
freehand = \"KeyF\"
crop = \"KeyC\"
select = \"KeyQ\"
").unwrap();
        assert_eq!(old.editor_keys_version, 0);
        let s = old.upgrade();
        assert_eq!(s.editor_binding(EditorAction::Freehand), "KeyW");
        assert_eq!(s.editor_binding(EditorAction::Crop), "KeyK");
        assert_eq!(s.editor_binding(EditorAction::Select), "KeyQ");
        assert_eq!(s.editor_binding(EditorAction::Asterisk), "KeyF");
        assert_eq!(s.editor_binding(EditorAction::Cone), "KeyC");
        // Someone who moved crop to X keeps it, and a second load changes nothing.
        let mine: Settings = toml::from_str("[editor_keys]
freehand = \"KeyP\"
crop = \"KeyX\"
").unwrap();
        let s = mine.upgrade();
        assert_eq!((s.editor_binding(EditorAction::Freehand), s.editor_binding(EditorAction::Crop)), ("KeyP", "KeyX"));
        let again: Settings = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(again.editor_keys_version, EDITOR_KEYS_VERSION);
    }

    #[test]
    fn old_files_migrate_to_destinations() {
        let old: Settings = toml::from_str("open_editor = false\nsave_to_file = true\ncopy_to_clipboard = false\n").unwrap();
        assert_eq!(old.migrate_destinations(), vec![DestId::File]);
        let editor: Settings = toml::from_str("open_editor = true\n").unwrap();
        assert_eq!(editor.migrate_destinations(), vec![DestId::Editor]);
        // The old keys are never written back.
        let text = toml::to_string(&Settings { destinations: Some(vec![DestId::File]), ..old }).unwrap();
        assert!(!text.contains("open_editor") && text.contains("destinations"));
    }

    #[test]
    fn the_tour_theme_round_trips_and_old_files_get_the_defaults() {
        let mut s = Settings::default();
        s.editor.tour_theme = SavedTheme { card_style: "light".into(), accent: "#6aa8f0".into(), hotspot: "dot".into(), progress_bar: false, backdrop: "#20252e".into(), spotlight_soft: false, zoom_enabled: false, zoom_percent: 130 };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(back.editor.tour_theme, s.editor.tour_theme);
        // A file from before guide cards: the defaults, with the calm green accent.
        let old: Settings = toml::from_str("[editor]\ntool = \"text\"\n").unwrap();
        assert_eq!(old.editor.tour_theme, SavedTheme::default());
        assert_eq!((old.editor.tour_theme.card_style.as_str(), old.editor.tour_theme.accent.as_str(), old.editor.tour_theme.zoom_percent), ("dark", "#5dcaa5", 118));
    }

    #[test]
    fn the_animated_hotspot_is_on_by_default_and_remembered() {
        let old: Settings = toml::from_str("[editor]\ntool = \"text\"\n").unwrap();
        assert!(old.editor.tour_animate_hotspot, "a file from before the switch animates");
        let mut s = Settings::default();
        s.editor.tour_animate_hotspot = false;
        let back: Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert!(!back.editor.tour_animate_hotspot);
    }

    #[test]
    fn recordings_are_kept_by_default_and_the_choice_is_remembered() {
        assert!(Settings::default().keep_published_recordings);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        // A file from before the switch existed, and an empty one: kept.
        std::fs::write(&path, "magnifier = false
publish_server = \"https://tours.example.com\"
").unwrap();
        let old = load_at(&path);
        assert!(old.load_problem.is_none() && old.keep_published_recordings && !old.magnifier);
        std::fs::write(&path, "").unwrap();
        assert!(load_at(&path).keep_published_recordings);
        // Turned off, saved and read again: off. Turned on again: on.
        let mut s = old;
        s.keep_published_recordings = false;
        s.save_to(&path).unwrap();
        let mut read = load_at(&path);
        assert!(read.load_problem.is_none() && !read.keep_published_recordings);
        read.keep_published_recordings = true;
        read.save_to(&path).unwrap();
        assert!(load_at(&path).keep_published_recordings);
    }

    #[test]
    fn recordings_are_stored_as_video_by_default_and_the_choice_is_remembered() {
        assert!(Settings::default().store_recordings_as_video);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        // A file from before the switch existed, and an empty one: on.
        std::fs::write(&path, "magnifier = false\nkeep_published_recordings = false\n").unwrap();
        let old = load_at(&path);
        assert!(old.load_problem.is_none() && old.store_recordings_as_video && !old.magnifier && !old.keep_published_recordings);
        std::fs::write(&path, "").unwrap();
        assert!(load_at(&path).store_recordings_as_video);
        // Turned off, saved and read again: off, under the name the file uses. Turned on again: on.
        let mut s = old;
        s.store_recordings_as_video = false;
        s.save_to(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("store_recordings_as_video = false"));
        let mut read = load_at(&path);
        assert!(read.load_problem.is_none() && !read.store_recordings_as_video);
        read.store_recordings_as_video = true;
        read.save_to(&path).unwrap();
        assert!(load_at(&path).store_recordings_as_video);
        // It is its own switch: keeping recordings is not touched by it.
        assert!(!load_at(&path).keep_published_recordings);
    }

    #[test]
    fn the_warning_is_5_gb_by_default_and_0_is_remembered_as_no_warning() {
        assert_eq!(Settings::default().saved_recordings_warn_gb, 5);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        // A file from before the limit existed, and an empty one: 5 GB.
        std::fs::write(&path, "magnifier = false\n").unwrap();
        let old = load_at(&path);
        assert!(old.load_problem.is_none() && old.saved_recordings_warn_gb == 5 && !old.magnifier);
        std::fs::write(&path, "").unwrap();
        assert_eq!(load_at(&path).saved_recordings_warn_gb, 5);
        // No warning is a choice, and does not read as the default the next time.
        let mut s = old;
        s.saved_recordings_warn_gb = 0;
        s.save_to(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("saved_recordings_warn_gb = 0"));
        let mut read = load_at(&path);
        assert_eq!((read.saved_recordings_warn_gb, &read.load_problem), (0, &None));
        // Every limit that can be typed is remembered, the smallest and the largest too.
        for gb in [1, 12, crate::project::LIMIT_MAX_GB] {
            read.saved_recordings_warn_gb = gb;
            read.save_to(&path).unwrap();
            let back = load_at(&path);
            assert_eq!((back.saved_recordings_warn_gb, &back.load_problem), (gb, &None));
        }
    }

    #[test]
    fn a_limit_in_the_file_that_is_not_a_whole_number_costs_only_that_value() {
        // Text, a decimal, a negative number, a switch, a list, and numbers no field could hold.
        for bad in ["\"lots\"", "\"5\"", "2.5", "-3", "true", "[5]", "10000", "4294967296", "99999999999"] {
            let text = format!("saved_recordings_warn_gb = {bad}\nstore_recordings_as_video = false\npublish_server = \"https://tours.example.com\"\nmagnifier = false\n\n[hotkeys]\ncapture_region = \"control+KeyQ\"\n");
            assert!(toml::from_str::<Settings>(&text).is_err(), "the strict parse refuses {bad}");
            let s = Settings::parse(&text).expect("still loads").upgrade();
            assert_eq!(s.saved_recordings_warn_gb, 5, "{bad} reads as the default");
            assert!(!s.store_recordings_as_video && !s.magnifier, "{bad}");
            assert_eq!((s.publish_server.as_str(), s.binding(Action::CaptureRegion)), ("https://tours.example.com", "control+KeyQ"), "{bad}");
        }
        // The same for the switch: a word where on or off belongs costs the switch, not the limit.
        let s = Settings::parse("store_recordings_as_video = \"yes\"\nsaved_recordings_warn_gb = 12\nkeep_published_recordings = false\n").unwrap();
        assert!(s.store_recordings_as_video && !s.keep_published_recordings);
        assert_eq!(s.saved_recordings_warn_gb, 12);
        // Through a real load: the file is copied aside as it was, and the rest of it is in use.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        let mine = "saved_recordings_warn_gb = 2.5\nstore_recordings_as_video = false\nmagnifier = false\n";
        std::fs::write(&path, mine).unwrap();
        let s = load_at(&path);
        let copy = tmp.path().join("settings.unreadable-20261118-093000.toml");
        assert_eq!(s.load_problem, Some(LoadProblem { path: path.clone(), backup: Some(copy.clone()), partly_read: true }));
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), mine);
        assert_eq!((s.saved_recordings_warn_gb, s.store_recordings_as_video, s.magnifier), (5, false, false));
        // The largest limit and no limit are whole numbers the file may hold.
        for (good, gb) in [("9999", 9999), ("0", 0), ("1", 1)] {
            let s: Settings = toml::from_str(&format!("saved_recordings_warn_gb = {good}\n")).unwrap();
            assert_eq!(s.saved_recordings_warn_gb, gb);
        }
    }

    #[test]
    fn the_tray_left_click_opens_the_app_by_default_and_is_remembered() {
        assert_eq!(Settings::default().tray_left_click, TrayLeftClick::OpenApp);
        // A file from before the main window has no such key.
        let old: Settings = toml::from_str("magnifier = false
publish_server = \"https://tours.example.com\"
").unwrap();
        assert_eq!(old.upgrade().tray_left_click, TrayLeftClick::OpenApp);
        let empty: Settings = toml::from_str("").unwrap();
        assert_eq!(empty.tray_left_click, TrayLeftClick::OpenApp);
        // Both values round-trip, under the names the file uses.
        for (choice, word) in [(TrayLeftClick::CaptureRegion, "capture_region"), (TrayLeftClick::OpenApp, "open_app")] {
            let s = Settings { tray_left_click: choice, ..Settings::default() };
            let text = toml::to_string_pretty(&s).unwrap();
            assert!(text.contains(&format!("tray_left_click = \"{word}\"")), "{text}");
            let back: Settings = toml::from_str(&text).unwrap();
            assert_eq!(back.tray_left_click, choice);
        }
        assert_eq!((TrayLeftClick::OpenApp.label(), TrayLeftClick::CaptureRegion.label()), ("Open Mythicshot", "Capture a region"));
    }

    #[test]
    fn an_unknown_value_costs_only_that_setting() {
        // A typo, or a choice from a newer version.
        let text = "tray_left_click = \"open_dashboard\"\noutput_dir = \"D:/Shots\"\npublish_server = \"https://tours.example.com\"\nmagnifier = false\n\n[hotkeys]\ncapture_region = \"control+KeyQ\"\n";
        assert!(toml::from_str::<Settings>(text).is_err(), "the strict parse refuses the file");
        let s = Settings::parse(text).expect("still loads").upgrade();
        assert_eq!(s.tray_left_click, TrayLeftClick::OpenApp);
        assert_eq!(s.output_dir, PathBuf::from("D:/Shots"));
        assert_eq!(s.publish_server, "https://tours.example.com");
        assert!(!s.magnifier);
        assert_eq!(s.binding(Action::CaptureRegion), "control+KeyQ");
        // The same for a value of the wrong kind.
        let s = Settings::parse("tray_left_click = 3\npublish_server = \"https://tours.example.com\"\n").unwrap();
        assert_eq!((s.tray_left_click, s.publish_server.as_str()), (TrayLeftClick::OpenApp, "https://tours.example.com"));
    }

    #[test]
    fn every_other_enum_valued_setting_is_as_forgiving() {
        // The image format, a destination, an overlay style, and an action in each shortcut section.
        let text = "image_format = \"avif\"\ndestinations = [\"file\", \"hologram\"]\njpeg_quality = \"high\"\npublish_server = \"https://tours.example.com\"\n\n[overlays]\nkeys = \"neon\"\npointer = false\n\n[hotkeys]\ncapture_window = \"alt+KeyW\"\nteleport = \"KeyT\"\n\n[editor_keys]\ncrop = \"KeyX\"\nsculpt = \"KeyS\"\n";
        assert!(toml::from_str::<Settings>(text).is_err());
        let s = Settings::parse(text).expect("still loads").upgrade();
        let d = Settings::default();
        assert_eq!(s.publish_server, "https://tours.example.com");
        assert_eq!((s.image_format, s.jpeg_quality), (d.image_format, d.jpeg_quality));
        assert_eq!(s.destinations(), &[DestId::File], "the destination it knows stays");
        // Sections keep their good entries.
        assert!(!s.overlays.pointer);
        assert_eq!(s.overlays.keys, OverlayStyle::default().keys, "as when the section never named it");
        assert_eq!(s.binding(Action::CaptureWindow), "alt+KeyW");
        assert_eq!(s.editor_binding(EditorAction::Crop), "KeyX");
        assert_eq!(s.binding(Action::CaptureRegion), d.binding(Action::CaptureRegion));
    }

    #[test]
    fn only_the_bad_value_is_lost_however_deep_it_sits() {
        // One bad number in one tool's style, one in the tour theme, one unknown entry in a list.
        let text = "destinations = [\"file\", \"hologram\", \"clipboard\"]\n\n[editor]\ntool = \"text\"\nexport_scale = \"huge\"\n\n[editor.styles.text]\nwidth = \"wide\"\nfont_size = 32.0\nshadow = false\n\n[editor.styles.arrow]\nwidth = 7.0\n\n[editor.tour_theme]\naccent = \"#6aa8f0\"\nzoom_percent = \"big\"\nhotspot = \"dot\"\n";
        assert!(toml::from_str::<Settings>(text).is_err());
        let s = Settings::parse(text).expect("still loads").upgrade();
        assert_eq!(s.destinations(), &[DestId::File, DestId::Clipboard], "only the unknown entry goes");
        assert_eq!(s.editor.tool, "text");
        assert_eq!(s.editor.export_scale, EditorPrefs::default().export_scale);
        let text_style = s.editor.styles["text"];
        assert_eq!((text_style.width, text_style.font_size, text_style.shadow), (SavedStyle::default().width, 32.0, false));
        assert_eq!(s.editor.styles["arrow"].width, 7.0, "the other tool's style is untouched");
        let theme = &s.editor.tour_theme;
        assert_eq!((theme.accent.as_str(), theme.hotspot.as_str(), theme.zoom_percent), ("#6aa8f0", "dot", SavedTheme::default().zoom_percent));
    }

    fn when() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap().and_hms_opt(9, 30, 0).unwrap()
    }

    fn load_at(path: &std::path::Path) -> Settings {
        Settings::load_with(path, |p| std::fs::read(p), |p, bytes| back_up(p, bytes, when()))
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_file_that_cannot_be_used_is_copied_aside_before_anything_is_saved() {
        // Not TOML, not text at all, and TOML with one value from a newer version.
        let cases: [(&[u8], bool); 3] = [(b"publish_server = \"https://tours.example.com\n[[[", false), (b"\xff\xfe\x00magnifier", false), (b"tray_left_click = \"open_dashboard\"\nmagnifier = false\n", true)];
        for (bytes, partly) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("settings.toml");
            std::fs::write(&path, bytes).unwrap();
            let s = load_at(&path);
            let copy = tmp.path().join("settings.unreadable-20261118-093000.toml");
            assert_eq!(s.load_problem, Some(LoadProblem { path: path.clone(), backup: Some(copy.clone()), partly_read: partly }));
            assert_eq!(std::fs::read(&copy).unwrap(), bytes, "the copy is the file as it was");
            assert_eq!(s.magnifier, !partly, "what could be read is kept, the rest is the default");
            // With the copy made, saving goes ahead.
            s.save_to(&path).unwrap();
            let saved = std::fs::read_to_string(&path).unwrap();
            assert!(saved.contains("tray_left_click = \"open_app\""), "{saved}");
            assert!(!saved.contains("load_problem") && !saved.contains("unreadable"), "the problem is never written: {saved}");
            let sentence = s.load_problem.as_ref().unwrap().sentence();
            assert!(sentence.contains(&copy.display().to_string()) && !sentence.contains("will not be saved"), "{sentence}");
        }
    }

    #[test]
    fn a_good_or_missing_file_has_no_problem_and_leaves_no_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "tray_left_click = \"capture_region\"\n").unwrap();
        let s = load_at(&path);
        assert_eq!((s.tray_left_click, &s.load_problem), (TrayLeftClick::CaptureRegion, &None));
        let first = load_at(&tmp.path().join("none.toml"));
        assert_eq!((first.tray_left_click, &first.load_problem), (TrayLeftClick::OpenApp, &None));
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        // A first start saves as usual.
        first.save_to(&tmp.path().join("none.toml")).unwrap();
        assert!(tmp.path().join("none.toml").exists());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_saved_over() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        let mine = "publish_server = \"https://tours.example.com\"\n[hotkeys]\ncapture_region = \"control+KeyQ\"\n";
        std::fs::write(&path, mine).unwrap();
        // Locked by another program, or no permission.
        let denied = |_: &std::path::Path| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let mut s = Settings::load_with(&path, denied, |p, bytes| back_up(p, bytes, when()));
        assert_eq!(s.load_problem, Some(LoadProblem { path: path.clone(), backup: None, partly_read: false }));
        assert_eq!(s.publish_server, "", "the defaults, for this session");
        // Changes made this session are not written, by this copy of the settings or a clone of it.
        s.magnifier = false;
        s.toggle_destination(DestId::File);
        s.save_to(&path).unwrap();
        s.clone().save_to(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
        assert_eq!(names(tmp.path()), ["settings.toml"], "and nothing else is written either");
        let sentence = s.load_problem.as_ref().unwrap().sentence();
        assert!(sentence.contains("changes will not be saved") && sentence.contains(&path.display().to_string()), "{sentence}");
        // A folder where the file should be reads as an error too, not as a first start.
        let odd = tmp.path().join("odd");
        std::fs::create_dir_all(odd.join("settings.toml")).unwrap();
        let s = load_at(&odd.join("settings.toml"));
        assert!(s.load_problem.as_ref().is_some_and(LoadProblem::keeps_file_untouched));
    }

    #[test]
    fn when_the_copy_cannot_be_made_the_file_is_never_saved_over() {
        let no_copy = |_: &std::path::Path, _: &[u8]| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        // Whether nothing could be read or only part of it.
        for (mine, partly) in [("publish_server = \"https://tours.example.com\n[[[", false), ("tray_left_click = \"open_dashboard\"\npublish_server = \"https://tours.example.com\"\n", true)] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("settings.toml");
            std::fs::write(&path, mine).unwrap();
            let mut s = Settings::load_with(&path, |p| std::fs::read(p), no_copy);
            assert_eq!(s.load_problem, Some(LoadProblem { path: path.clone(), backup: None, partly_read: partly }));
            assert_eq!(s.publish_server, if partly { "https://tours.example.com" } else { "" });
            s.magnifier = !s.magnifier;
            s.save_to(&path).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
            assert_eq!(names(tmp.path()), ["settings.toml"]);
        }
    }

    #[test]
    fn each_copy_has_its_own_name_and_only_the_newest_five_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        // Files that only look similar are never removed.
        for other in ["settings.unreadable.toml", "settings.unreadable-notes.toml", "settings.unreadable-20260101-000000.toml.bak", "settings.unreadable-20260101-000000-x.toml"] {
            std::fs::write(tmp.path().join(other), "mine").unwrap();
        }
        // Two in the same second do not overwrite each other.
        let a = back_up(&path, b"first", when()).unwrap();
        let b = back_up(&path, b"second", when()).unwrap();
        assert_eq!((a.file_name().unwrap().to_str().unwrap(), b.file_name().unwrap().to_str().unwrap()), ("settings.unreadable-20261118-093000.toml", "settings.unreadable-20261118-093000-2.toml"));
        assert_eq!((std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap()), (b"first".to_vec(), b"second".to_vec()));
        for minute in 31..37 {
            back_up(&path, b"later", when().date().and_hms_opt(9, minute, 0).unwrap()).unwrap();
        }
        assert_eq!(
            names(tmp.path()),
            [
                "settings.unreadable-20260101-000000-x.toml",
                "settings.unreadable-20260101-000000.toml.bak",
                "settings.unreadable-20261118-093200.toml",
                "settings.unreadable-20261118-093300.toml",
                "settings.unreadable-20261118-093400.toml",
                "settings.unreadable-20261118-093500.toml",
                "settings.unreadable-20261118-093600.toml",
                "settings.unreadable-notes.toml",
                "settings.unreadable.toml",
            ]
        );
        assert_eq!(backup_stamp("settings.unreadable-20261118-093000-2.toml"), Some(("20261118-093000".into(), 2)));
        assert_eq!(backup_stamp("settings.toml"), None);
    }

    #[test]
    fn a_rename_takes_the_place_of_a_file_that_is_there() {
        // What the save relies on, on this system and this toolchain.
        let tmp = tempfile::tempdir().unwrap();
        let (old, new) = (tmp.path().join("settings.toml"), tmp.path().join("settings.toml.tmp"));
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        std::fs::rename(&new, &old).unwrap();
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "new");
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        assert_eq!(temp_path(&old), new);
    }

    #[test]
    fn a_save_writes_beside_the_file_first_and_then_takes_its_place() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "magnifier = true\n").unwrap();
        // The new text is whole in the temporary file, and the old file still whole, when the rename is asked for.
        let seen = std::cell::RefCell::new(None);
        let write = |to: &std::path::Path| std::fs::write(to, "magnifier = false\n");
        replace_with(&path, write, |from, to| {
            *seen.borrow_mut() = Some((from.to_path_buf(), std::fs::read_to_string(from).unwrap(), std::fs::read_to_string(to).unwrap()));
            std::fs::rename(from, to)
        }, 1, || ())
        .unwrap();
        assert_eq!(seen.into_inner(), Some((temp_path(&path), "magnifier = false\n".to_string(), "magnifier = true\n".to_string())));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "magnifier = false\n");
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        // The app's save and a job's note both go that way, and leave nothing else behind.
        let mut s = load_at(&path);
        s.magnifier = true;
        s.save_to(&path).unwrap();
        assert!(Settings::patch_unfinished_deletes(&path, &[UnfinishedDelete { server: "https://tours.example.com".into(), tour_id: "tour_bbbbbbbbbbbbbbbbbbbbbbbb".into(), title: "Testing".into() }]));
        let read = load_at(&path);
        assert!(read.magnifier && read.unfinished_deletes.len() == 1 && read.load_problem.is_none());
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        // No file yet, in a folder that is not there yet.
        let fresh = tmp.path().join("new").join("settings.toml");
        write_file(&fresh, "magnifier = false\n").unwrap();
        assert_eq!(names(fresh.parent().unwrap()), ["settings.toml"]);
    }

    #[test]
    fn a_write_or_a_rename_that_fails_leaves_the_file_as_it_was_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        let mine = "publish_server = \"https://tours.example.com\"\nmagnifier = false\n";
        std::fs::write(&path, mine).unwrap();
        // The disk fills half-way through the new text.
        let full = |to: &std::path::Path| {
            std::fs::write(to, "publish_ser")?;
            Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
        };
        let e = replace_with(&path, full, |from, to| std::fs::rename(from, to), 1, || ()).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::StorageFull);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
        assert_eq!(names(tmp.path()), ["settings.toml"], "the half-written file is removed");
        // The new text is written, but cannot take the file's place.
        let whole = |to: &std::path::Path| std::fs::write(to, "magnifier = true\n");
        let e = replace_with(&path, whole, |_, _| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)), 1, || ()).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        // For real: the temporary file cannot be made, because a folder has its name.
        std::fs::create_dir(temp_path(&path)).unwrap();
        assert!(write_file(&path, "magnifier = true\n").is_err());
        assert!(!Settings::patch_unfinished_deletes(&path, &[]), "a note that is not on disk is reported as not written");
        let mut s = load_at(&path);
        s.magnifier = true;
        assert!(s.save_to(&path).is_err(), "the app is told, so that it keeps the change to save it again");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
        assert!(temp_path(&path).is_dir(), "what is not the app's own file is not removed");
    }

    fn os_error(code: i32) -> std::io::Error {
        std::io::Error::from_raw_os_error(code)
    }

    #[test]
    fn a_rename_that_is_refused_for_a_moment_is_tried_again() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        let mine = "magnifier = false\n";
        std::fs::write(&path, mine).unwrap();
        let whole = |to: &std::path::Path| std::fs::write(to, "magnifier = true\n");
        // Another program has the file for a moment: no permission twice, a sharing violation once, then it is free.
        let (asked, waits) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
        let busy_then_free = |from: &std::path::Path, to: &std::path::Path| {
            asked.set(asked.get() + 1);
            match asked.get() {
                1 => Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                2 => Err(os_error(5)),
                3 => Err(os_error(32)),
                _ => std::fs::rename(from, to),
            }
        };
        replace_with(&path, whole, busy_then_free, 5, || waits.set(waits.get() + 1)).unwrap();
        assert_eq!((asked.get(), waits.get()), (4, 3), "a wait before each further try, none after the last");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "magnifier = true\n");
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        // It never comes free: the tries end, the error is the last one, and the file is as it was.
        std::fs::write(&path, mine).unwrap();
        let (asked, waits) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
        let never = |_: &std::path::Path, _: &std::path::Path| {
            asked.set(asked.get() + 1);
            Err(os_error(32))
        };
        let e = replace_with(&path, whole, never, 5, || waits.set(waits.get() + 1)).unwrap_err();
        assert_eq!((e.raw_os_error(), asked.get(), waits.get()), (Some(32), 5, 4));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        // An error that waiting does not cure is not tried again.
        let (asked, waits) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
        let gone = |_: &std::path::Path, _: &std::path::Path| {
            asked.set(asked.get() + 1);
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        };
        let e = replace_with(&path, whole, gone, 5, || waits.set(waits.get() + 1)).unwrap_err();
        assert_eq!((e.kind(), asked.get(), waits.get()), (std::io::ErrorKind::NotFound, 1, 0));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
        // The app's own save waits half a second at most.
        assert_eq!(RENAME_TRIES, 5);
        assert!(RENAME_WAIT * (RENAME_TRIES - 1) <= std::time::Duration::from_millis(500));
    }

    /// The write step of a save, as the app does it.
    #[cfg(windows)]
    fn whole_file(text: &'static str) -> impl FnOnce(&std::path::Path) -> std::io::Result<()> {
        move |to| std::fs::write(to, text)
    }

    #[cfg(windows)]
    #[test]
    fn a_read_only_settings_file_is_not_replaced_and_stays_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        let mine = "publish_server = \"https://tours.example.com\"\n";
        std::fs::write(&path, mine).unwrap();
        let read_only = |on: bool| {
            let mut p = std::fs::metadata(&path).unwrap().permissions();
            p.set_readonly(on);
            std::fs::set_permissions(&path, p).unwrap();
        };
        read_only(true);
        // The real rename, refused by the system every time; only the waiting is left out.
        let waits = std::cell::Cell::new(0);
        let result = replace_with(&path, whole_file("magnifier = false\n"), |from, to| std::fs::rename(from, to), RENAME_TRIES, || waits.set(waits.get() + 1));
        let left = (std::fs::read_to_string(&path).unwrap(), names(tmp.path()));
        // So that the folder can be removed again.
        read_only(false);
        let e = result.unwrap_err();
        assert!(is_busy(&e), "{e:?}");
        assert_eq!(waits.get(), RENAME_TRIES - 1);
        assert_eq!(left, (mine.to_string(), vec!["settings.toml".to_string()]), "the file is as it was, and no temporary file is left");
        // Once it may be written again, the next save goes through.
        write_file(&path, "magnifier = false\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "magnifier = false\n");
    }

    #[cfg(windows)]
    #[test]
    fn a_temporary_file_that_cannot_be_removed_does_not_stop_the_next_save() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        let mine = "magnifier = false\n";
        std::fs::write(&path, mine).unwrap();
        // Another program (a scanner) opens the fresh temporary file and lets nobody move or remove it.
        let held = std::cell::RefCell::new(None);
        let rename = |from: &std::path::Path, to: &std::path::Path| {
            if held.borrow().is_none() {
                // Shared for reading and writing, not for deleting.
                *held.borrow_mut() = Some(std::fs::OpenOptions::new().read(true).share_mode(3).open(from)?);
            }
            std::fs::rename(from, to)
        };
        let e = replace_with(&path, whole_file("magnifier = true\npublish_server = \"https://tours.example.com\"\n"), rename, RENAME_TRIES, || ()).unwrap_err();
        assert!(is_busy(&e), "{e:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
        assert_eq!(names(tmp.path()), ["settings.toml", "settings.toml.tmp"], "it could not be removed");
        // The scanner is done. The leftover is longer than the next text: it is written over from its start.
        drop(held.into_inner());
        write_file(&path, "magnifier = true\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "magnifier = true\n");
        assert_eq!(names(tmp.path()), ["settings.toml"]);
    }

    #[test]
    fn a_save_that_failed_is_tried_again_later_and_not_at_once() {
        let start = std::time::Instant::now();
        let mut retry = SaveRetry::default();
        assert!(!retry.pending() && retry.allows(start));
        // A save that worked leaves nothing to do.
        retry.record(&Ok(()), start);
        assert!(!retry.pending() && retry.allows(start));
        // One that failed is pending, and is not tried again frame after frame.
        retry.record(&Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)), start);
        assert!(retry.pending());
        assert!(!retry.allows(start) && !retry.allows(start + SAVE_RETRY_AFTER - std::time::Duration::from_millis(1)));
        assert!(retry.allows(start + SAVE_RETRY_AFTER));
        // It fails again: the wait starts again. Then it works.
        let later = start + SAVE_RETRY_AFTER;
        retry.record(&Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)), later);
        assert!(retry.pending() && !retry.allows(later + std::time::Duration::from_secs(1)));
        retry.record(&Ok(()), later + SAVE_RETRY_AFTER);
        assert!(!retry.pending() && retry.allows(later + SAVE_RETRY_AFTER));
    }

    #[test]
    fn a_job_may_write_its_note_only_to_a_file_that_may_be_saved() {
        let path = PathBuf::from("settings.toml");
        assert!(Settings::default().note_saver().is_some(), "a first start, or a file that read well");
        // Copied aside before it is saved over: saving goes ahead, for a note too.
        let copied = Settings { load_problem: Some(LoadProblem { path: path.clone(), backup: Some(PathBuf::from("settings.unreadable-20261118-093000.toml")), partly_read: true }), ..Settings::default() };
        assert!(copied.note_saver().is_some());
        // Unreadable, or no copy could be made: never written over, by a job either.
        for partly_read in [false, true] {
            let untouched = Settings { load_problem: Some(LoadProblem { path: path.clone(), backup: None, partly_read }), ..Settings::default() };
            assert!(untouched.note_saver().is_none());
            assert!(untouched.save_to(&PathBuf::from("no-such-folder").join("settings.toml")).is_ok(), "nothing is to be saved, so nothing is left to try again");
        }
    }

    #[test]
    fn a_temporary_file_left_by_a_crash_is_written_over_and_never_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        let note = UnfinishedDelete { server: "https://tours.example.com".into(), tour_id: "tour_bbbbbbbbbbbbbbbbbbbbbbbb".into(), title: "Testing".into() };
        std::fs::write(&path, "magnifier = false\n").unwrap();
        // The app stopped half-way through a save: part of a file, longer than the next one.
        let stale = format!("magnifier = true\npublish_server = \"https://tours.exa{}", "x".repeat(4000));
        std::fs::write(temp_path(&path), &stale).unwrap();
        // The next start reads the settings file alone, finds nothing wrong, and copies nothing aside.
        let mut s = load_at(&path);
        assert_eq!((s.magnifier, &s.load_problem), (false, &None));
        assert_eq!(names(tmp.path()), ["settings.toml", "settings.toml.tmp"]);
        // A job's note, then the app's save: each writes over the leftover and takes it away.
        assert!(Settings::patch_unfinished_deletes(&path, std::slice::from_ref(&note)));
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        std::fs::write(temp_path(&path), &stale).unwrap();
        s.unfinished_deletes = vec![note.clone()];
        s.output_dir = PathBuf::from("D:/Shots");
        s.save_to(&path).unwrap();
        assert_eq!(names(tmp.path()), ["settings.toml"]);
        let read = load_at(&path);
        assert_eq!((read.magnifier, read.output_dir.clone(), read.unfinished_deletes.clone(), &read.load_problem), (false, PathBuf::from("D:/Shots"), vec![note], &None));
        // A leftover and no settings file at all is a first start.
        let first = tmp.path().join("first");
        std::fs::create_dir(&first).unwrap();
        std::fs::write(temp_path(&first.join("settings.toml")), &stale).unwrap();
        let s = load_at(&first.join("settings.toml"));
        assert_eq!((s.magnifier, &s.load_problem), (true, &None));
        s.save_to(&first.join("settings.toml")).unwrap();
        assert_eq!(names(&first), ["settings.toml"]);
    }

    #[test]
    fn toggling_keeps_menu_order() {
        let mut s = Settings { destinations: Some(vec![DestId::Clipboard]), ..Settings::default() };
        s.toggle_destination(DestId::File);
        assert_eq!(s.destinations(), &[DestId::File, DestId::Clipboard]);
        s.toggle_destination(DestId::Clipboard);
        assert_eq!(s.destinations(), &[DestId::File]);
    }

    #[test]
    fn a_note_written_at_once_changes_only_the_unfinished_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        let note = UnfinishedDelete { server: "https://tours.example.com".into(), tour_id: "tour_bbbbbbbbbbbbbbbbbbbbbbbb".into(), title: "Testing \"quoted\" · café".into() };
        // Into an existing file: every other value stays, a key of a newer version included.
        let mut s = Settings { publish_server: "https://tours.example.com".into(), publish_saved_for: Some("https://tours.example.com".into()), publish_binding_settled: true, ..Settings::default() };
        s.hotkeys.insert(Action::CaptureWindow, "alt+KeyW".into());
        std::fs::write(&path, format!("from_a_newer_version = 7
{}", toml::to_string_pretty(&s).unwrap())).unwrap();
        assert!(Settings::patch_unfinished_deletes(&path, std::slice::from_ref(&note)));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("from_a_newer_version = 7") && !text.contains("dp_"), "{text}");
        let read = load_at(&path);
        assert!(read.load_problem.is_none());
        assert_eq!(read.unfinished_deletes, [note.clone()]);
        assert_eq!((read.publish_server.as_str(), read.publish_saved_for.as_deref(), read.publish_binding_settled, read.binding(Action::CaptureWindow)), ("https://tours.example.com", Some("https://tours.example.com"), true, "alt+KeyW"));
        // The app's own save afterwards, and an emptied list.
        read.save_to(&path).unwrap();
        assert_eq!(load_at(&path).unfinished_deletes, [note.clone()]);
        assert!(Settings::patch_unfinished_deletes(&path, &[]));
        assert!(load_at(&path).unfinished_deletes.is_empty());
        // No file yet: the note is still on disk before the request.
        let fresh = tmp.path().join("new").join("settings.toml");
        assert!(Settings::patch_unfinished_deletes(&fresh, std::slice::from_ref(&note)));
        assert_eq!(load_at(&fresh).unfinished_deletes, [note.clone()]);
        // A file that is not TOML is left exactly as it is.
        std::fs::write(&path, "not = [toml").unwrap();
        assert!(!Settings::patch_unfinished_deletes(&path, &[note]));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not = [toml");
    }
}
