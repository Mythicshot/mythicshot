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
}

impl Action {
    pub const ALL: &'static [Action] = &[Action::CaptureRegion, Action::CaptureLastRegion, Action::CaptureWindow, Action::CaptureFullScreen, Action::RecordRegion];

    pub fn label(self) -> &'static str {
        match self {
            Action::CaptureRegion => "Capture region",
            Action::CaptureLastRegion => "Capture last region",
            Action::CaptureWindow => "Capture window",
            Action::CaptureFullScreen => "Capture full screen",
            Action::RecordRegion => "Record region",
        }
    }

    pub fn group(self) -> &'static str {
        match self {
            Action::RecordRegion => "Recording",
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
        }
    }
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
        Self { manager, active: HashMap::new(), by_id: HashMap::new(), errors: HashMap::new() }
    }

    pub fn action_for(&self, id: u32) -> Option<Action> {
        self.by_id.get(&id).copied()
    }

    /// Register `binding` for `action`, replacing its old shortcut. An empty
    /// binding just removes the old one. On failure the action is left with no
    /// shortcut and the reason is kept in `errors`.
    pub fn apply(&mut self, action: Action, binding: &str) {
        if let Some(old) = self.active.remove(&action) {
            if let Some(m) = &self.manager {
                let _ = m.unregister(old);
            }
            self.by_id.remove(&old.id());
        }
        self.errors.remove(&action);
        if binding.is_empty() {
            return;
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
