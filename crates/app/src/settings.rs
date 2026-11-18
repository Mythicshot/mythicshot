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
            load_problem: None,
            open_editor: None,
            save_to_file: None,
            copy_to_clipboard: None,
        }
    }
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

    pub fn save(&self) {
        self.save_to(&Self::path());
    }

    /// Write the settings to `path`, unless the file there could not be read at start-up and no
    /// copy of it could be made: then it is never written over.
    fn save_to(&self, path: &std::path::Path) {
        if self.load_problem.as_ref().is_some_and(LoadProblem::keeps_file_untouched) {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(path, text) {
                    eprintln!("could not save settings: {e}");
                }
            }
            Err(e) => eprintln!("could not encode settings: {e}"),
        }
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
            s.save_to(&path);
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
        first.save_to(&tmp.path().join("none.toml"));
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
        s.save_to(&path);
        s.clone().save_to(&path);
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
            s.save_to(&path);
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
    fn toggling_keeps_menu_order() {
        let mut s = Settings { destinations: Some(vec![DestId::Clipboard]), ..Settings::default() };
        s.toggle_destination(DestId::File);
        assert_eq!(s.destinations(), &[DestId::File, DestId::Clipboard]);
        s.toggle_destination(DestId::Clipboard);
        assert_eq!(s.destinations(), &[DestId::File]);
    }
}
