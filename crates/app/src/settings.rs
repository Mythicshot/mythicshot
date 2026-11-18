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
        let parsed: Option<Settings> = std::fs::read_to_string(Self::path()).ok().and_then(|text| toml::from_str(&text).ok());
        parsed.unwrap_or_default().upgrade()
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
    fn toggling_keeps_menu_order() {
        let mut s = Settings { destinations: Some(vec![DestId::Clipboard]), ..Settings::default() };
        s.toggle_destination(DestId::File);
        assert_eq!(s.destinations(), &[DestId::File, DestId::Clipboard]);
        s.toggle_destination(DestId::Clipboard);
        assert_eq!(s.destinations(), &[DestId::File]);
    }
}
