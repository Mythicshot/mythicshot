//! Turns raw input-hook events into overlay events while recording.
//!
//! The hook thread calls [`Tap::convert`] for every key and button event and
//! stamps it with the recording clock right there, so events line up with
//! frames even when the UI thread is busy. Clicks are always logged (just the
//! button and where it was); keys only when the user turned "Record keys" on,
//! and never while a password field has focus (see [`crate::password`]).

use mythicshot_editor::overlays::{InputEvent, InputKind, MouseButton};
use mythicshot_recording::ClockHandle;
use rdev::{Button, EventType, Key};

use crate::password::PasswordWatch;

pub struct Tap {
    pub clock: ClockHandle,
    /// Top-left of the recorded area in desktop pixels.
    pub origin: (i32, i32),
    /// Live draw is on: its clicks and keys are not part of the overlay.
    pub drawing: bool,
    /// Running while keys are logged.
    password: Option<PasswordWatch>,
    /// Keys whose press was logged; only their releases are logged, so a
    /// release never gives away a key pressed in a password field.
    down: Vec<Key>,
}

impl Tap {
    pub fn new(clock: ClockHandle, origin: (i32, i32), keys: bool) -> Self {
        let mut tap = Self { clock, origin, drawing: false, password: None, down: Vec::new() };
        tap.set_keys(keys);
        tap
    }

    pub fn set_keys(&mut self, on: bool) {
        if on != self.password.is_some() {
            self.password = on.then(PasswordWatch::start);
            self.down.clear();
        }
    }

    /// An overlay event for `ev`, or `None` if it should not be logged.
    /// `pointer` is the last mouse position the hook saw.
    pub fn convert(&mut self, ev: &EventType, pointer: (f64, f64)) -> Option<InputEvent> {
        if self.clock.paused() || self.drawing {
            return None;
        }
        let (x, y) = (pointer.0 as i32 - self.origin.0, pointer.1 as i32 - self.origin.1);
        let kind = match ev {
            EventType::KeyPress(k) => {
                if self.password.as_ref().is_none_or(PasswordWatch::blocked) {
                    return None;
                }
                let label = key_label(*k)?;
                if !self.down.contains(k) {
                    self.down.push(*k);
                }
                InputKind::KeyDown(label)
            }
            EventType::KeyRelease(k) => {
                let i = self.down.iter().position(|d| d == k)?;
                self.down.swap_remove(i);
                InputKind::KeyUp(key_label(*k)?)
            }
            EventType::ButtonPress(b) => InputKind::MouseDown(button(*b)?, x, y),
            EventType::ButtonRelease(b) => InputKind::MouseUp(button(*b)?, x, y),
            _ => return None,
        };
        Some(InputEvent { t_ms: self.clock.active_ms(), kind })
    }
}

fn button(b: Button) -> Option<MouseButton> {
    match b {
        Button::Left => Some(MouseButton::Left),
        Button::Right => Some(MouseButton::Right),
        Button::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// The label a key gets in chips and on the on-screen keyboard.
pub fn key_label(k: Key) -> Option<String> {
    use Key::*;
    let s = match k {
        ControlLeft | ControlRight => "Ctrl",
        ShiftLeft | ShiftRight => "Shift",
        Alt | AltGr => "Alt",
        MetaLeft | MetaRight => "Win",
        Return | KpReturn => "Enter",
        Escape => "Esc",
        Space => "Space",
        Tab => "Tab",
        Backspace => "Bksp",
        Delete | KpDelete => "Del",
        Insert => "Ins",
        Home => "Home",
        End => "End",
        PageUp => "PgUp",
        PageDown => "PgDn",
        UpArrow => "Up",
        DownArrow => "Down",
        LeftArrow => "Left",
        RightArrow => "Right",
        CapsLock => "Caps",
        PrintScreen => "PrtSc",
        Pause => "Pause",
        BackQuote => "`",
        Minus | KpMinus => "-",
        Equal => "=",
        LeftBracket => "[",
        RightBracket => "]",
        SemiColon => ";",
        Quote => "'",
        BackSlash | IntlBackslash => "\\",
        Comma => ",",
        Dot => ".",
        Slash | KpDivide => "/",
        KpPlus => "+",
        KpMultiply => "*",
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
        Num0 | Kp0 => "0",
        Num1 | Kp1 => "1",
        Num2 | Kp2 => "2",
        Num3 | Kp3 => "3",
        Num4 | Kp4 => "4",
        Num5 | Kp5 => "5",
        Num6 | Kp6 => "6",
        Num7 | Kp7 => "7",
        Num8 | Kp8 => "8",
        Num9 | Kp9 => "9",
        KeyA => "A",
        KeyB => "B",
        KeyC => "C",
        KeyD => "D",
        KeyE => "E",
        KeyF => "F",
        KeyG => "G",
        KeyH => "H",
        KeyI => "I",
        KeyJ => "J",
        KeyK => "K",
        KeyL => "L",
        KeyM => "M",
        KeyN => "N",
        KeyO => "O",
        KeyP => "P",
        KeyQ => "Q",
        KeyR => "R",
        KeyS => "S",
        KeyT => "T",
        KeyU => "U",
        KeyV => "V",
        KeyW => "W",
        KeyX => "X",
        KeyY => "Y",
        KeyZ => "Z",
        Unknown(code @ 124..=135) => return Some(format!("F{}", code - 111)),
        _ => return None,
    };
    Some(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_the_on_screen_keyboard() {
        assert_eq!(key_label(Key::ControlRight).as_deref(), Some("Ctrl"));
        assert_eq!(key_label(Key::Backspace).as_deref(), Some("Bksp"));
        assert_eq!(key_label(Key::KeyS).as_deref(), Some("S"));
        assert_eq!(key_label(Key::Unknown(124)).as_deref(), Some("F13"));
    }
}
