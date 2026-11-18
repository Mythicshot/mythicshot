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

    // Phase 1–2 options, read once to fill `destinations`, never written.
    #[serde(skip_serializing)]
    open_editor: Option<bool>,
    #[serde(skip_serializing)]
    save_to_file: Option<bool>,
    #[serde(skip_serializing)]
    copy_to_clipboard: Option<bool>,
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
        Self::load_from(&Self::path())
    }

    /// Read the settings file at `path`. A file that is not TOML at all cannot be used; since the
    /// next change saves over it, a copy is kept beside it first (`settings.unreadable.toml`).
    fn load_from(path: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else { return Settings::default().upgrade() };
        match Self::parse(&text) {
            Some(s) => s.upgrade(),
            None => {
                eprintln!("settings file {} could not be read; starting from the defaults", path.display());
                let _ = std::fs::copy(path, path.with_file_name("settings.unreadable.toml"));
                Settings::default().upgrade()
            }
        }
    }

    /// Settings from the file's text. A value this version does not understand (a mistyped word, a
    /// choice or an action from a newer version, a number where text belongs) costs only that
    /// one setting, which reads as its default; every other setting is kept. Without this, one
    /// such value made the whole file read as the defaults, and the next save wrote them over it.
    /// `None` only when the text is not TOML.
    fn parse(text: &str) -> Option<Settings> {
        if let Ok(s) = toml::from_str::<Settings>(text) {
            return Some(s);
        }
        let mut table: toml::Table = toml::from_str(text).ok()?;
        // Whether the file would load with just this one key (or one entry of a section) in it.
        let alone = |key: &str, value: toml::Value| {
            let mut one = toml::Table::new();
            one.insert(key.to_string(), value);
            one.try_into::<Settings>().is_ok()
        };
        let keys: Vec<String> = table.keys().cloned().collect();
        for key in keys {
            let value = table[&key].clone();
            if alone(&key, value.clone()) {
                continue;
            }
            // A section (hotkeys, editor, overlays): keep its good entries.
            let kept = match value {
                toml::Value::Table(section) => {
                    let good: toml::Table = section.into_iter().filter(|(k, v)| alone(&key, toml::Value::Table(toml::Table::from_iter([(k.clone(), v.clone())])))).collect();
                    Some(toml::Value::Table(good)).filter(|v| alone(&key, v.clone()))
                }
                _ => None,
            };
            eprintln!("settings: \"{key}\" has a value this version does not understand; using the default for it");
            match kept {
                Some(v) => table.insert(key, v),
                None => table.remove(&key),
            };
        }
        table.try_into().ok()
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
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
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
        assert_eq!(s.destinations(), d.destinations(), "a list with an unknown destination reads as the default list");
        // Sections keep their good entries.
        assert!(!s.overlays.pointer);
        assert_eq!(s.overlays.keys, OverlayStyle::default().keys, "as when the section never named it");
        assert_eq!(s.binding(Action::CaptureWindow), "alt+KeyW");
        assert_eq!(s.editor_binding(EditorAction::Crop), "KeyX");
        assert_eq!(s.binding(Action::CaptureRegion), d.binding(Action::CaptureRegion));
    }

    #[test]
    fn a_file_that_is_not_toml_is_copied_aside_before_the_defaults_are_used() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "publish_server = \"https://tours.example.com\n[[[").unwrap();
        assert!(Settings::parse(&std::fs::read_to_string(&path).unwrap()).is_none());
        let s = Settings::load_from(&path);
        assert_eq!(s.publish_server, "");
        assert_eq!(std::fs::read_to_string(tmp.path().join("settings.unreadable.toml")).unwrap(), std::fs::read_to_string(&path).unwrap());
        // A good file and a missing one leave nothing beside them.
        let good = tempfile::tempdir().unwrap();
        std::fs::write(good.path().join("settings.toml"), "tray_left_click = \"capture_region\"\n").unwrap();
        assert_eq!(Settings::load_from(&good.path().join("settings.toml")).tray_left_click, TrayLeftClick::CaptureRegion);
        assert_eq!(Settings::load_from(&good.path().join("none.toml")).tray_left_click, TrayLeftClick::OpenApp);
        assert_eq!(std::fs::read_dir(good.path()).unwrap().count(), 1);
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
