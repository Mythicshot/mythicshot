//! Settings file: `settings.toml` in the OS config folder
//! (`%APPDATA%\Mythicshot\config` on Windows).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hotkeys::{Action, EditorAction};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub output_dir: PathBuf,
    pub copy_to_clipboard: bool,
    pub save_to_file: bool,
    pub magnifier: bool,
    /// Last captured region in desktop pixels: x, y, width, height.
    pub last_region: Option<[i32; 4]>,
    /// Action → shortcut in `global-hotkey` syntax, for example `shift+PrintScreen`.
    /// An empty string means the action has no shortcut.
    pub hotkeys: BTreeMap<Action, String>,
    /// Open each capture in the editor instead of saving it straight away.
    pub open_editor: bool,
    /// Editor action → shortcut, same syntax as `hotkeys`.
    pub editor_keys: BTreeMap<EditorAction, String>,
}

impl Default for Settings {
    fn default() -> Self {
        let pictures = directories::UserDirs::new()
            .and_then(|d| d.picture_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            output_dir: pictures.join("Mythicshot"),
            copy_to_clipboard: true,
            save_to_file: true,
            magnifier: true,
            last_region: None,
            hotkeys: Action::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect(),
            open_editor: true,
            editor_keys: EditorAction::ALL.iter().map(|a| (*a, a.default_binding().to_string())).collect(),
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        directories::ProjectDirs::from("", "", "Mythicshot")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
            .join("settings.toml")
    }

    pub fn load() -> Self {
        let mut s: Settings = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        // Before recording existed, Shift+PrtSc was the default for last region.
        // Files from then get it moved to Record region, the new default.
        if !s.hotkeys.contains_key(&Action::RecordRegion) && s.binding(Action::CaptureLastRegion).eq_ignore_ascii_case("shift+PrintScreen") {
            s.hotkeys.insert(Action::CaptureLastRegion, String::new());
        }
        for a in Action::ALL {
            s.hotkeys.entry(*a).or_insert_with(|| a.default_binding().to_string());
        }
        for a in EditorAction::ALL {
            s.editor_keys.entry(*a).or_insert_with(|| a.default_binding().to_string());
        }
        s
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
}
