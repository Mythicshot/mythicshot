//! The system-wide key and mouse hook.
//!
//! On Windows this is our own low-level hook. rdev's version looks up each
//! key's character by attaching to the foreground window's input queue; when
//! the foreground window is one of ours (the recording bar after a click) that
//! stalls, Windows times the hook out, and key presses stop arriving. We only
//! need key codes, so this hook never touches another thread. Other systems use
//! rdev. Either way the callback gets rdev event types.
//!
//! On Windows the hook also takes Print Screen shortcuts before any other app
//! sees them (see [`set_print_screen`]): Windows hands a registered shortcut to
//! whichever app asked first, and at sign-in OneDrive, Snipping Tool or a
//! graphics driver's tool often beat Mythicshot to it.

use rdev::EventType;

use crate::hotkeys::Action;

/// Print Screen shortcuts the hook takes: modifiers (see [`MODS`]) → action.
/// `on_press` runs on the hook thread and must return quickly; it returns
/// false to let the key through (while a new shortcut is being recorded).
#[cfg(windows)]
pub fn set_print_screen(claims: Vec<(u8, Action)>, on_press: impl Fn(Action) -> bool + Send + Sync + 'static) {
    win::set_print_screen(claims, Box::new(on_press));
}

#[cfg(not(windows))]
pub fn set_print_screen(_claims: Vec<(u8, Action)>, _on_press: impl Fn(Action) -> bool + Send + Sync + 'static) {}

/// Modifier bits for [`set_print_screen`].
pub mod mods {
    pub const CTRL: u8 = 1;
    pub const SHIFT: u8 = 2;
    pub const ALT: u8 = 4;
    pub const WIN: u8 = 8;
}

/// Run the hook on the current thread until the process ends.
#[cfg(windows)]
pub fn listen(callback: impl FnMut(EventType) + Send + 'static) -> Result<(), String> {
    win::listen(Box::new(callback))
}

#[cfg(not(windows))]
pub fn listen(mut callback: impl FnMut(EventType) + Send + 'static) -> Result<(), String> {
    rdev::listen(move |e| callback(e.event_type)).map_err(|e| format!("{e:?}"))
}

#[cfg(windows)]
mod win {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock, RwLock};

    use rdev::{Button, EventType, Key};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};

    use super::mods;
    use crate::hotkeys::Action;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    type Callback = Box<dyn FnMut(EventType) + Send>;
    static CALLBACK: OnceLock<Mutex<Callback>> = OnceLock::new();

    struct PrintScreen {
        claims: Vec<(u8, Action)>,
        on_press: Box<dyn Fn(Action) -> bool + Send + Sync>,
    }
    static PRINT_SCREEN: RwLock<Option<PrintScreen>> = RwLock::new(None);
    /// A Print Screen press was taken, so its release is taken too.
    static TOOK_PRESS: AtomicBool = AtomicBool::new(false);
    const VK_SNAPSHOT: u32 = 0x2C;

    pub fn set_print_screen(claims: Vec<(u8, Action)>, on_press: Box<dyn Fn(Action) -> bool + Send + Sync>) {
        if let Ok(mut p) = PRINT_SCREEN.write() {
            *p = Some(PrintScreen { claims, on_press });
        }
    }

    fn held_mods() -> u8 {
        let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
        let mut m = 0;
        if down(VK_CONTROL.0) {
            m |= mods::CTRL;
        }
        if down(VK_SHIFT.0) {
            m |= mods::SHIFT;
        }
        if down(VK_MENU.0) {
            m |= mods::ALT;
        }
        if down(VK_LWIN.0) || down(VK_RWIN.0) {
            m |= mods::WIN;
        }
        m
    }

    /// Take a Print Screen key event if it is one of Mythicshot's shortcuts.
    fn take_print_screen(pressed: bool) -> bool {
        if !pressed {
            return TOOK_PRESS.swap(false, Ordering::Relaxed);
        }
        let Ok(guard) = PRINT_SCREEN.read() else { return false };
        let Some(p) = guard.as_ref() else { return false };
        let held = held_mods();
        match p.claims.iter().find(|(m, _)| *m == held) {
            Some((_, action)) if (p.on_press)(*action) => {
                TOOK_PRESS.store(true, Ordering::Relaxed);
                true
            }
            Some(_) => false,
            None => false,
        }
    }

    fn emit(ev: EventType) {
        if let Some(cb) = CALLBACK.get() {
            if let Ok(mut cb) = cb.lock() {
                cb(ev);
            }
        }
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if info.vkCode == VK_SNAPSHOT {
                let pressed = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                if take_print_screen(pressed) {
                    // Swallowed: no other app, and not Windows, sees this key.
                    return LRESULT(1);
                }
            }
            let key = vk_to_key(info.vkCode);
            match wparam.0 as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => emit(EventType::KeyPress(key)),
                WM_KEYUP | WM_SYSKEYUP => emit(EventType::KeyRelease(key)),
                _ => {}
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let ev = match wparam.0 as u32 {
                WM_MOUSEMOVE => Some(EventType::MouseMove { x: info.pt.x as f64, y: info.pt.y as f64 }),
                WM_LBUTTONDOWN => Some(EventType::ButtonPress(Button::Left)),
                WM_LBUTTONUP => Some(EventType::ButtonRelease(Button::Left)),
                WM_RBUTTONDOWN => Some(EventType::ButtonPress(Button::Right)),
                WM_RBUTTONUP => Some(EventType::ButtonRelease(Button::Right)),
                WM_MBUTTONDOWN => Some(EventType::ButtonPress(Button::Middle)),
                WM_MBUTTONUP => Some(EventType::ButtonRelease(Button::Middle)),
                _ => None,
            };
            if let Some(ev) = ev {
                // Buttons carry no position in rdev's types; send the position first.
                if !matches!(ev, EventType::MouseMove { .. }) {
                    emit(EventType::MouseMove { x: info.pt.x as f64, y: info.pt.y as f64 });
                }
                emit(ev);
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    pub fn listen(callback: Callback) -> Result<(), String> {
        CALLBACK.set(Mutex::new(callback)).map_err(|_| "the input hook is already running".to_string())?;
        unsafe {
            let module = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module.into()), 0).map_err(|e| format!("keyboard hook: {e}"))?;
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module.into()), 0).map_err(|e| format!("mouse hook: {e}"))?;
            // Low-level hooks run inside this thread's message loop.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        }
        Ok(())
    }

    /// Windows virtual-key code to rdev's key names.
    fn vk_to_key(vk: u32) -> Key {
        use Key::*;
        match vk {
            0x08 => Backspace,
            0x09 => Tab,
            0x0D => Return,
            0x10 | 0xA0 => ShiftLeft,
            0xA1 => ShiftRight,
            0x11 | 0xA2 => ControlLeft,
            0xA3 => ControlRight,
            0x12 | 0xA4 => Alt,
            0xA5 => AltGr,
            0x13 => Pause,
            0x14 => CapsLock,
            0x1B => Escape,
            0x20 => Space,
            0x21 => PageUp,
            0x22 => PageDown,
            0x23 => End,
            0x24 => Home,
            0x25 => LeftArrow,
            0x26 => UpArrow,
            0x27 => RightArrow,
            0x28 => DownArrow,
            0x2C => PrintScreen,
            0x2D => Insert,
            0x2E => Delete,
            0x30 => Num0,
            0x31 => Num1,
            0x32 => Num2,
            0x33 => Num3,
            0x34 => Num4,
            0x35 => Num5,
            0x36 => Num6,
            0x37 => Num7,
            0x38 => Num8,
            0x39 => Num9,
            0x41 => KeyA,
            0x42 => KeyB,
            0x43 => KeyC,
            0x44 => KeyD,
            0x45 => KeyE,
            0x46 => KeyF,
            0x47 => KeyG,
            0x48 => KeyH,
            0x49 => KeyI,
            0x4A => KeyJ,
            0x4B => KeyK,
            0x4C => KeyL,
            0x4D => KeyM,
            0x4E => KeyN,
            0x4F => KeyO,
            0x50 => KeyP,
            0x51 => KeyQ,
            0x52 => KeyR,
            0x53 => KeyS,
            0x54 => KeyT,
            0x55 => KeyU,
            0x56 => KeyV,
            0x57 => KeyW,
            0x58 => KeyX,
            0x59 => KeyY,
            0x5A => KeyZ,
            0x5B => MetaLeft,
            0x5C => MetaRight,
            0x60 => Kp0,
            0x61 => Kp1,
            0x62 => Kp2,
            0x63 => Kp3,
            0x64 => Kp4,
            0x65 => Kp5,
            0x66 => Kp6,
            0x67 => Kp7,
            0x68 => Kp8,
            0x69 => Kp9,
            0x6A => KpMultiply,
            0x6B => KpPlus,
            0x6D => KpMinus,
            0x6E => KpDelete,
            0x6F => KpDivide,
            0x70 => F1,
            0x71 => F2,
            0x72 => F3,
            0x73 => F4,
            0x74 => F5,
            0x75 => F6,
            0x76 => F7,
            0x77 => F8,
            0x78 => F9,
            0x79 => F10,
            0x7A => F11,
            0x7B => F12,
            0x91 => ScrollLock,
            0xBA => SemiColon,
            0xBB => Equal,
            0xBC => Comma,
            0xBD => Minus,
            0xBE => Dot,
            0xBF => Slash,
            0xC0 => BackQuote,
            0xDB => LeftBracket,
            0xDC => BackSlash,
            0xDD => RightBracket,
            0xDE => Quote,
            0xE2 => IntlBackslash,
            // F13–F24 and anything else keep their code, like rdev does.
            other => Unknown(other),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn key_codes() {
            assert_eq!(vk_to_key(0x53), Key::KeyS);
            assert_eq!(vk_to_key(0xA3), Key::ControlRight);
            assert_eq!(vk_to_key(0x7E), Key::Unknown(0x7E));
        }
    }
}
