//! Global shortcuts: the actions that can be bound, how bindings are shown,
//! and registering them with the OS.

use std::collections::HashMap;
use std::str::FromStr;

use global_hotkey::GlobalHotKeyManager;
use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    CaptureRegion,
    CaptureLastRegion,
    CaptureWindow,
    CaptureFullScreen,
    RecordRegion,
    RecordLastRegion,
    RecordWindow,
    RecordFullScreen,
    PauseRecording,
    LiveDraw,
}

impl Action {
    pub const ALL: &'static [Action] = &[
        Action::CaptureRegion,
        Action::CaptureLastRegion,
        Action::CaptureWindow,
        Action::CaptureFullScreen,
        Action::RecordRegion,
        Action::RecordLastRegion,
        Action::RecordWindow,
        Action::RecordFullScreen,
        Action::PauseRecording,
        Action::LiveDraw,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::CaptureRegion => "Capture region",
            Action::CaptureLastRegion => "Capture last region",
            Action::CaptureWindow => "Capture window",
            Action::CaptureFullScreen => "Capture full screen",
            Action::RecordRegion => "Record region (press again to stop)",
            Action::RecordLastRegion => "Record last region",
            Action::RecordWindow => "Record window",
            Action::RecordFullScreen => "Record full screen",
            Action::PauseRecording => "Pause or resume recording",
            Action::LiveDraw => "Draw on screen while recording",
        }
    }

    pub fn group(self) -> &'static str {
        match self {
            Action::RecordRegion | Action::RecordLastRegion | Action::RecordWindow | Action::RecordFullScreen | Action::PauseRecording | Action::LiveDraw => "Recording",
            _ => "Screenshot",
        }
    }

    pub fn default_binding(self) -> &'static str {
        match self {
            Action::CaptureRegion => "PrintScreen",
            Action::CaptureLastRegion => "",
            Action::CaptureWindow => "alt+PrintScreen",
            Action::CaptureFullScreen => "control+PrintScreen",
            Action::RecordRegion => "shift+PrintScreen",
            Action::RecordLastRegion | Action::RecordWindow | Action::RecordFullScreen => "",
            Action::PauseRecording => "Pause",
            Action::LiveDraw => "control+shift+KeyD",
        }
    }
}

/// Shortcuts inside the editor window. Unlike [`Action`] these are not
/// registered with the OS; the editor checks them only while it has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorAction {
    Select,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Freehand,
    Circle,
    Group,
    Cone,
    Slice,
    Text,
    Bubble,
    Step,
    Highlight,
    Obfuscate,
    Spotlight,
    Card,
    Crop,
    Picture,
    Undo,
    Redo,
    Delete,
    Copy,
    Cut,
    Paste,
    Duplicate,
    Save,
}

impl EditorAction {
    pub const ALL: &'static [EditorAction] = &[
        EditorAction::Select,
        EditorAction::Rectangle,
        EditorAction::Ellipse,
        EditorAction::Line,
        EditorAction::Arrow,
        EditorAction::Freehand,
        EditorAction::Circle,
        EditorAction::Group,
        EditorAction::Cone,
        EditorAction::Slice,
        EditorAction::Text,
        EditorAction::Bubble,
        EditorAction::Step,
        EditorAction::Highlight,
        EditorAction::Obfuscate,
        EditorAction::Spotlight,
        EditorAction::Card,
        EditorAction::Crop,
        EditorAction::Picture,
        EditorAction::Undo,
        EditorAction::Redo,
        EditorAction::Delete,
        EditorAction::Copy,
        EditorAction::Cut,
        EditorAction::Paste,
        EditorAction::Duplicate,
        EditorAction::Save,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EditorAction::Select => "Select tool",
            EditorAction::Rectangle => "Rectangle tool",
            EditorAction::Ellipse => "Ellipse tool",
            EditorAction::Line => "Line tool",
            EditorAction::Arrow => "Arrow tool",
            EditorAction::Freehand => "Pen (freehand) tool",
            EditorAction::Circle => "Circle tool",
            EditorAction::Group => "Group tool (dashed area)",
            EditorAction::Cone => "Cone tool",
            EditorAction::Slice => "Pizza slice tool",
            EditorAction::Text => "Text tool",
            EditorAction::Bubble => "Speech bubble tool",
            EditorAction::Step => "Step label tool",
            EditorAction::Highlight => "Highlight tool",
            EditorAction::Obfuscate => "Obfuscate tool",
            EditorAction::Spotlight => "Spotlight tool (recordings)",
            EditorAction::Card => "Guide card tool (Tour view)",
            EditorAction::Crop => "Crop tool",
            EditorAction::Picture => "Picture tool",
            EditorAction::Undo => "Undo",
            EditorAction::Redo => "Redo",
            EditorAction::Delete => "Delete selected",
            EditorAction::Copy => "Copy the selected annotation, or the image",
            EditorAction::Cut => "Cut the selected annotation",
            EditorAction::Paste => "Paste an annotation",
            EditorAction::Duplicate => "Duplicate the selected annotation",
            EditorAction::Save => "Save",
        }
    }

    pub fn default_binding(self) -> &'static str {
        match self {
            EditorAction::Select => "KeyV",
            EditorAction::Rectangle => "KeyR",
            EditorAction::Ellipse => "KeyE",
            EditorAction::Line => "KeyL",
            EditorAction::Arrow => "KeyA",
            EditorAction::Freehand => "KeyW",
            EditorAction::Circle => "KeyO",
            EditorAction::Group => "KeyG",
            EditorAction::Cone => "KeyC",
            EditorAction::Slice => "KeyU",
            EditorAction::Text => "KeyT",
            EditorAction::Bubble => "KeyS",
            EditorAction::Step => "KeyN",
            EditorAction::Highlight => "KeyH",
            EditorAction::Obfuscate => "KeyB",
            // Unbound until the user picks a key.
            EditorAction::Spotlight => "",
            EditorAction::Card => "",
            EditorAction::Crop => "KeyK",
            // An action, not a mode: unbound until the user picks a key (Z, F and D stay free).
            EditorAction::Picture => "",
            EditorAction::Undo => "control+KeyZ",
            EditorAction::Redo => "control+KeyY",
            EditorAction::Delete => "Delete",
            EditorAction::Copy => "control+KeyC",
            EditorAction::Cut => "control+KeyX",
            EditorAction::Paste => "control+KeyV",
            EditorAction::Duplicate => "control+KeyD",
            EditorAction::Save => "control+KeyS",
        }
    }
}

/// A shortcut string in egui terms, for matching key presses in a window.
pub fn egui_shortcut(binding: &str) -> Option<(eframe::egui::Modifiers, eframe::egui::Key)> {
    use eframe::egui::{Key, Modifiers};
    let mut mods = Modifiers::NONE;
    let mut key = None;
    for part in binding.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" | "option" => mods.alt = true,
            "meta" | "super" | "cmd" | "command" => mods.mac_cmd = true,
            _ => {
                let name = part.strip_prefix("Key").or_else(|| part.strip_prefix("Digit")).unwrap_or(part);
                key = Key::from_name(name);
            }
        }
    }
    if mods.ctrl {
        mods.command = true;
    }
    key.map(|k| (mods, k))
}

/// `control+shift+KeyS` → `Ctrl+Shift+S`, the way the menu and settings show it.
pub fn display(binding: &str) -> String {
    if binding.is_empty() {
        return String::new();
    }
    let mut mods = Vec::new();
    let mut key = String::new();
    for part in binding.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => mods.push("Ctrl"),
            "shift" => mods.push("Shift"),
            "alt" | "option" => mods.push("Alt"),
            "meta" | "super" | "cmd" | "command" => mods.push(if cfg!(target_os = "macos") { "Cmd" } else { "Win" }),
            _ => key = key_name(part),
        }
    }
    let order = ["Ctrl", "Alt", "Shift", "Win", "Cmd"];
    mods.sort_by_key(|m| order.iter().position(|o| o == m));
    mods.push(&key);
    mods.join("+")
}

fn key_name(code: &str) -> String {
    let c = code.trim();
    match c.to_ascii_lowercase().as_str() {
        "printscreen" => return "PrtSc".into(),
        "pause" => return "Pause".into(),
        "scrolllock" => return "ScrLk".into(),
        "pageup" => return "PgUp".into(),
        "pagedown" => return "PgDn".into(),
        "arrowup" => return "Up".into(),
        "arrowdown" => return "Down".into(),
        "arrowleft" => return "Left".into(),
        "arrowright" => return "Right".into(),
        "backquote" => return "`".into(),
        _ => {}
    }
    if let Some(rest) = c.strip_prefix("Key") {
        return rest.into();
    }
    if let Some(rest) = c.strip_prefix("Digit") {
        return rest.into();
    }
    c.into()
}

pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    active: HashMap<Action, HotKey>,
    by_id: HashMap<u32, Action>,
    /// Why an action's shortcut could not be registered.
    pub errors: HashMap<Action, String>,
    /// Print Screen shortcuts go to the input hook instead of being
    /// registered (Windows only).
    pub take_print_screen: bool,
    /// Shortcuts the input hook takes: modifiers and virtual-key code, and
    /// the binding, per action.
    taken: HashMap<Action, ((u8, u32), String)>,
    /// A recording is running, so recording shortcuts are live.
    recording: bool,
}

const VK_SNAPSHOT: u32 = 0x2C;

impl Action {
    /// Only used while recording. On Windows the input hook takes these keys
    /// during a recording, so no other app can hold them, and leaves them to
    /// other apps the rest of the time.
    pub fn while_recording(self) -> bool {
        matches!(self, Action::PauseRecording | Action::LiveDraw)
    }
}

/// A shortcut as [`crate::hook::mods`] bits and a Windows virtual-key code,
/// or `None` for keys the hook doesn't know.
pub fn binding_vk(binding: &str) -> Option<(u8, u32)> {
    use crate::hook::mods;
    let mut m = 0;
    let mut vk = None;
    for part in binding.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => m |= mods::CTRL,
            "shift" => m |= mods::SHIFT,
            "alt" | "option" => m |= mods::ALT,
            "meta" | "super" | "cmd" | "command" => m |= mods::WIN,
            _ => vk = Some(key_vk(part)?),
        }
    }
    vk.map(|vk| (m, vk))
}

fn key_vk(name: &str) -> Option<u32> {
    let n = name.trim();
    let one = |s: &str| s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric();
    if let Some(c) = n.strip_prefix("Key").filter(|c| one(c)) {
        return Some(c.to_ascii_uppercase().as_bytes()[0] as u32);
    }
    if let Some(c) = n.strip_prefix("Digit").filter(|c| one(c)) {
        return Some(c.as_bytes()[0] as u32);
    }
    if let Some(f) = n.strip_prefix('F').and_then(|f| f.parse::<u32>().ok()).filter(|f| (1..=24).contains(f)) {
        return Some(0x6F + f);
    }
    Some(match n.to_ascii_lowercase().as_str() {
        "printscreen" => VK_SNAPSHOT,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "arrowleft" => 0x25,
        "arrowup" => 0x26,
        "arrowright" => 0x27,
        "arrowdown" => 0x28,
        "space" => 0x20,
        _ => return None,
    })
}

/// The modifiers of a Print Screen shortcut, or `None` if `binding` is not on
/// the Print Screen key.
pub fn print_screen_mods(binding: &str) -> Option<u8> {
    binding_vk(binding).filter(|(_, vk)| *vk == VK_SNAPSHOT).map(|(m, _)| m)
}

impl Hotkeys {
    pub fn new() -> Self {
        let manager = match GlobalHotKeyManager::new() {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("global shortcuts unavailable: {e}");
                None
            }
        };
        Self { manager, active: HashMap::new(), by_id: HashMap::new(), errors: HashMap::new(), take_print_screen: false, taken: HashMap::new(), recording: false }
    }

    /// The shortcuts the input hook should take now: Print Screen ones always,
    /// recording ones only while a recording is running.
    pub fn taken(&self) -> Vec<crate::hook::TakenKey> {
        self.taken.iter().filter(|(a, _)| self.recording || !a.while_recording()).map(|(a, ((m, vk), _))| (*m, *vk, *a)).collect()
    }

    /// A recording started or ended: recording shortcuts go live or are given
    /// back to other apps.
    pub fn set_recording(&mut self, on: bool) {
        self.recording = on;
        let recording_keys: Vec<(Action, String)> = self.taken.iter().filter(|(a, _)| a.while_recording()).map(|(a, (_, b))| (*a, b.clone())).collect();
        for (action, binding) in recording_keys {
            if on {
                self.register_quietly(action, &binding);
            } else {
                self.unregister(action);
            }
        }
    }

    fn unregister(&mut self, action: Action) {
        if let Some(old) = self.active.remove(&action) {
            if let Some(m) = &self.manager {
                let _ = m.unregister(old);
            }
            self.by_id.remove(&old.id());
        }
    }

    /// Ask Windows for a shortcut the hook also takes, without reporting a
    /// failure. The hook doesn't see keys while one of Mythicshot's own
    /// windows has focus, and a registered shortcut does; when another app
    /// holds it, the hook still covers every other window.
    fn register_quietly(&mut self, action: Action, binding: &str) {
        let (Ok(hotkey), Some(m)) = (HotKey::from_str(binding), &self.manager) else { return };
        if !self.by_id.contains_key(&hotkey.id()) && m.register(hotkey).is_ok() {
            self.active.insert(action, hotkey);
            self.by_id.insert(hotkey.id(), action);
        }
    }

    /// Whether the input hook handles `action`'s shortcut.
    pub fn is_taken(&self, action: Action) -> bool {
        self.taken.contains_key(&action)
    }

    pub fn action_for(&self, id: u32) -> Option<Action> {
        self.by_id.get(&id).copied()
    }

    /// Register `binding` for `action`, replacing its old shortcut. An empty
    /// binding just removes the old one. On failure the action is left with no
    /// shortcut and the reason is kept in `errors`.
    pub fn apply(&mut self, action: Action, binding: &str) {
        self.unregister(action);
        self.errors.remove(&action);
        self.taken.remove(&action);
        if binding.is_empty() {
            return;
        }
        if cfg!(windows) {
            if let Some(key) = binding_vk(binding).filter(|(_, vk)| action.while_recording() || (self.take_print_screen && *vk == VK_SNAPSHOT)) {
                if let Some((other, _)) = self.taken.iter().find(|(_, (k, _))| *k == key) {
                    self.errors.insert(action, format!("Already used by \"{}\"", other.label()));
                    return;
                }
                self.taken.insert(action, (key, binding.to_string()));
                if !action.while_recording() || self.recording {
                    self.register_quietly(action, binding);
                }
                return;
            }
        }
        let hotkey = match HotKey::from_str(binding) {
            Ok(h) => h,
            Err(e) => {
                self.errors.insert(action, format!("Not a valid shortcut: {e}"));
                return;
            }
        };
        if let Some(other) = self.by_id.get(&hotkey.id()) {
            self.errors.insert(action, format!("Already used by \"{}\"", other.label()));
            return;
        }
        let Some(m) = &self.manager else {
            self.errors.insert(action, "Global shortcuts are not available on this system".into());
            return;
        };
        match m.register(hotkey) {
            Ok(()) => {
                self.active.insert(action, hotkey);
                self.by_id.insert(hotkey.id(), action);
            }
            Err(_) => {
                self.errors.insert(action, "Windows or another app already uses this shortcut".into());
            }
        }
    }
}

/// Turns raw key events from the input hook into a shortcut string while the
/// settings page is listening for a new shortcut.
#[derive(Default)]
pub struct ShortcutRecorder {
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
}

pub enum Recorded {
    Pending,
    Cancel,
    Clear,
    Done(String),
}

impl ShortcutRecorder {
    pub fn feed(&mut self, ev: &rdev::EventType) -> Recorded {
        use rdev::{EventType, Key};
        let (key, down) = match ev {
            EventType::KeyPress(k) => (*k, true),
            EventType::KeyRelease(k) => (*k, false),
            _ => return Recorded::Pending,
        };
        match key {
            Key::ControlLeft | Key::ControlRight => self.ctrl = down,
            Key::ShiftLeft | Key::ShiftRight => self.shift = down,
            Key::Alt | Key::AltGr => self.alt = down,
            Key::MetaLeft | Key::MetaRight => self.meta = down,
            _ if !down => {}
            Key::Escape if !self.any_mod() => return Recorded::Cancel,
            Key::Backspace if !self.any_mod() => return Recorded::Clear,
            other => {
                if let Some(code) = code_name(other) {
                    let mut s = String::new();
                    if self.ctrl {
                        s.push_str("control+");
                    }
                    if self.alt {
                        s.push_str("alt+");
                    }
                    if self.shift {
                        s.push_str("shift+");
                    }
                    if self.meta {
                        s.push_str("super+");
                    }
                    s.push_str(&code);
                    return Recorded::Done(s);
                }
            }
        }
        Recorded::Pending
    }

    fn any_mod(&self) -> bool {
        self.ctrl || self.shift || self.alt || self.meta
    }
}

fn code_name(key: rdev::Key) -> Option<String> {
    use rdev::Key::*;
    let name = match key {
        PrintScreen => "PrintScreen",
        Pause => "Pause",
        ScrollLock => "ScrollLock",
        Insert => "Insert",
        Delete => "Delete",
        Home => "Home",
        End => "End",
        PageUp => "PageUp",
        PageDown => "PageDown",
        UpArrow => "ArrowUp",
        DownArrow => "ArrowDown",
        LeftArrow => "ArrowLeft",
        RightArrow => "ArrowRight",
        Space => "Space",
        Tab => "Tab",
        Return => "Enter",
        BackQuote => "Backquote",
        Minus => "Minus",
        Equal => "Equal",
        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        F5 => "F5",
        F6 => "F6",
        F7 => "F7",
        F8 => "F8",
        F9 => "F9",
        F10 => "F10",
        F11 => "F11",
        F12 => "F12",
        Num0 => "Digit0",
        Num1 => "Digit1",
        Num2 => "Digit2",
        Num3 => "Digit3",
        Num4 => "Digit4",
        Num5 => "Digit5",
        Num6 => "Digit6",
        Num7 => "Digit7",
        Num8 => "Digit8",
        Num9 => "Digit9",
        KeyA => "KeyA",
        KeyB => "KeyB",
        KeyC => "KeyC",
        KeyD => "KeyD",
        KeyE => "KeyE",
        KeyF => "KeyF",
        KeyG => "KeyG",
        KeyH => "KeyH",
        KeyI => "KeyI",
        KeyJ => "KeyJ",
        KeyK => "KeyK",
        KeyL => "KeyL",
        KeyM => "KeyM",
        KeyN => "KeyN",
        KeyO => "KeyO",
        KeyP => "KeyP",
        KeyQ => "KeyQ",
        KeyR => "KeyR",
        KeyS => "KeyS",
        KeyT => "KeyT",
        KeyU => "KeyU",
        KeyV => "KeyV",
        KeyW => "KeyW",
        KeyX => "KeyX",
        KeyY => "KeyY",
        KeyZ => "KeyZ",
        Unknown(code @ 124..=135) => return Some(format!("F{}", code - 111)),
        _ => return None,
    };
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_screen_shortcuts_go_to_the_hook() {
        use crate::hook::mods;
        assert_eq!(print_screen_mods("PrintScreen"), Some(0));
        assert_eq!(print_screen_mods("alt+PrintScreen"), Some(mods::ALT));
        assert_eq!(print_screen_mods("control+shift+PrintScreen"), Some(mods::CTRL | mods::SHIFT));
        assert_eq!(print_screen_mods("control+KeyS"), None);
        let mut h = Hotkeys { manager: None, active: HashMap::new(), by_id: HashMap::new(), errors: HashMap::new(), take_print_screen: true, taken: HashMap::new(), recording: false };
        h.apply(Action::CaptureRegion, "PrintScreen");
        h.apply(Action::CaptureWindow, "alt+PrintScreen");
        h.apply(Action::CaptureFullScreen, "PrintScreen");
        if cfg!(windows) {
            // Taken by the hook, so no "another app uses this" error; a clash
            // between two of our own actions is still reported.
            assert!(!h.errors.contains_key(&Action::CaptureRegion) && !h.errors.contains_key(&Action::CaptureWindow));
            assert!(h.errors[&Action::CaptureFullScreen].contains("Capture region"));
            let mut t = h.taken();
            t.sort_by_key(|(m, _, _)| *m);
            assert_eq!(t, vec![(0, 0x2C, Action::CaptureRegion), (mods::ALT, 0x2C, Action::CaptureWindow)]);
            // Recording keys go to the hook too, but only while recording.
            h.apply(Action::LiveDraw, "control+shift+KeyD");
            h.apply(Action::PauseRecording, "Pause");
            assert!(h.errors.get(&Action::LiveDraw).is_none() && h.errors.get(&Action::PauseRecording).is_none());
            assert!(h.taken().iter().all(|(_, _, a)| !a.while_recording()));
            h.set_recording(true);
            let rec = h.taken();
            assert!(rec.contains(&(mods::CTRL | mods::SHIFT, 0x44, Action::LiveDraw)) && rec.contains(&(0, 0x13, Action::PauseRecording)));
        }
        // Moving an action off Print Screen frees its slot.
        h.apply(Action::CaptureRegion, "");
        assert!(h.taken().iter().all(|(_, _, a)| *a != Action::CaptureRegion));
        // Keys the hook knows.
        assert_eq!(binding_vk("control+shift+KeyD"), Some((mods::CTRL | mods::SHIFT, 0x44)));
        assert_eq!(binding_vk("F13"), Some((0, 0x7C)));
        assert_eq!(binding_vk("alt+Digit5"), Some((mods::ALT, 0x35)));
        assert_eq!(binding_vk("control+Quote"), None);
    }

    #[test]
    fn display_names() {
        assert_eq!(display("PrintScreen"), "PrtSc");
        assert_eq!(display("shift+control+KeyS"), "Ctrl+Shift+S");
        assert_eq!(display("alt+PrintScreen"), "Alt+PrtSc");
        assert_eq!(display(""), "");
    }

    #[test]
    fn defaults_parse() {
        for a in Action::ALL.iter().filter(|a| !a.default_binding().is_empty()) {
            HotKey::from_str(a.default_binding()).expect("default binding parses");
        }
    }

    #[test]
    fn editor_defaults_parse_for_egui() {
        for a in EditorAction::ALL.iter().filter(|a| !a.default_binding().is_empty()) {
            assert!(egui_shortcut(a.default_binding()).is_some(), "{a:?}");
        }
        let (m, k) = egui_shortcut("control+KeyZ").unwrap();
        assert!(m.ctrl && k == eframe::egui::Key::Z);
    }

    #[test]
    fn recorder_builds_shortcut() {
        use rdev::{EventType::*, Key};
        let mut r = ShortcutRecorder::default();
        assert!(matches!(r.feed(&KeyPress(Key::ControlLeft)), Recorded::Pending));
        assert!(matches!(r.feed(&KeyPress(Key::ShiftLeft)), Recorded::Pending));
        match r.feed(&KeyPress(Key::Num4)) {
            Recorded::Done(s) => {
                assert_eq!(s, "control+shift+Digit4");
                HotKey::from_str(&s).expect("recorded shortcut parses");
            }
            _ => panic!("expected a shortcut"),
        }
    }
}
