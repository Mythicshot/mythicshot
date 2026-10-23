//! Settings file: `settings.toml` in the OS config folder
//! (`%APPDATA%\Mythicshot\config` on Windows).

use std::collections::BTreeMap;
use std::path::PathBuf;

use mythicshot_destinations::{Context, DEFAULT_PATTERN, DestId, ImageFormat};
use serde::{Deserialize, Serialize};

use crate::hotkeys::{Action, EditorAction};

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
    /// Last captured region in desktop pixels: x, y, width, height.
    pub last_region: Option<[i32; 4]>,
    /// Action → shortcut in `global-hotkey` syntax, for example `shift+PrintScreen`.
    /// An empty string means the action has no shortcut.
    pub hotkeys: BTreeMap<Action, String>,
    /// Editor action → shortcut, same syntax as `hotkeys`.
    pub editor_keys: BTreeMap<EditorAction, String>,

    // Phase 1–2 options, read once to fill `destinations`, never written.
    #[serde(skip_serializing)]
    open_editor: Option<bool>,
    #[serde(skip_serializing)]
    save_to_file: Option<bool>,
    #[serde(skip_serializing)]
    copy_to_clipboard: Option<bool>,
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
            last_region: None,
            hotkeys: Action::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect(),
            editor_keys: EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect(),
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
        let mut s = parsed.unwrap_or_default();
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
    fn toggling_keeps_menu_order() {
        let mut s = Settings { destinations: Some(vec![DestId::Clipboard]), ..Settings::default() };
        s.toggle_destination(DestId::File);
        assert_eq!(s.destinations(), &[DestId::File, DestId::Clipboard]);
        s.toggle_destination(DestId::Clipboard);
        assert_eq!(s.destinations(), &[DestId::File]);
    }
}
