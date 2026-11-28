//! The system-wide key and mouse hook.
//!
//! On Windows this is our own low-level hook. rdev's version looks up each
//! key's character by attaching to the foreground window's input queue; when
//! the foreground window is one of ours (the recording bar after a click) that
//! stalls, Windows times the hook out, and key presses stop arriving. We only
//! need key codes, so this hook never touches another thread. Other systems use
//! rdev. Either way the callback gets rdev event types.
//!
//! On Windows the hook also takes some shortcuts before any other app sees
//! them (see [`set_taken_keys`]): Windows hands a registered shortcut to
//! whichever app asked first, and at sign-in OneDrive, Snipping Tool or a
//! graphics driver's tool often beat Mythicshot to Print Screen.

use rdev::EventType;

use crate::crash_log::HookKind;
use crate::hotkeys::Action;

/// A shortcut the hook takes: modifier bits (see [`mods`]), a Windows
/// virtual-key code, and the action it runs.
pub type TakenKey = (u8, u32, Action);

/// The shortcuts the hook takes from now on. `on_press` runs on the hook
/// thread and must return quickly; it returns false to let the key through
/// (while a new shortcut is being recorded).
#[cfg(windows)]
pub fn set_taken_keys(keys: Vec<TakenKey>, on_press: impl Fn(Action) -> bool + Send + Sync + 'static) {
    win::set_taken_keys(keys, Box::new(on_press));
}

#[cfg(not(windows))]
pub fn set_taken_keys(_keys: Vec<TakenKey>, _on_press: impl Fn(Action) -> bool + Send + Sync + 'static) {}

/// Modifier bits for [`TakenKey`].
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

/// Run one hook callback of `kind`, catching a panic in it: false (the event is not taken) when it
/// panicked. While it runs the panic hook knows it is in a hook callback, and writes only one short
/// line (see [`crate::crash_log::in_hook`]).
#[cfg_attr(not(windows), allow(dead_code))]
fn guarded(kind: HookKind, f: impl FnOnce() -> bool) -> bool {
    crate::crash_log::in_hook(kind, || std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(false))
}

/// What a hook procedure answers Windows: 1 when the callback took the event (swallowed), else
/// what the next hook answers, which passes the event on; a callback that panicked passes it on.
#[cfg_attr(not(windows), allow(dead_code))]
fn answer(kind: HookKind, action: bool, event: impl FnOnce() -> bool, next: impl FnOnce() -> isize) -> isize {
    if action && guarded(kind, event) { 1 } else { next() }
}

/// The lock, also when a callback panicked while it held it.
#[cfg_attr(not(windows), allow(dead_code))]
fn lock_or_recover<T: ?Sized>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Checks of the hook against Windows' own record of input are this far apart at least.
pub const CHECK_EVERY_MS: u32 = 1000;
/// Input this much newer than the hook's last event is input the hook did not see.
pub const LAG_MS: u32 = 3000;
/// Checks in a row, each with new input the hook did not see, before the hook is put in again.
pub const STRIKES: u8 = 3;
/// The hook is put in again at most this often.
pub const REINSTALL_EVERY_MS: u32 = 60_000;

/// Whether the hook still gets input. Windows removes a low-level hook that answers too slowly
/// and says nothing; after that no shortcut works. Windows' record of the last input (keyboard
/// and mouse) is compared with the hook's last event (keyboard and mouse alike): newer input,
/// arriving afresh on [`STRIKES`] checks in a row and each time more than [`LAG_MS`] past the
/// hook's last event, means the hook is gone. Times are Windows' millisecond ticks, which wrap.
#[derive(Debug, Default)]
pub struct Liveness {
    last_check: Option<u32>,
    last_system: Option<u32>,
    strikes: u8,
    last_reinstall: Option<u32>,
}

/// What a check of the hook calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Fine,
    /// Put the hook in again: Windows has input `lag_ms` newer than the hook's last event.
    Reinstall { lag_ms: u32 },
}

impl Liveness {
    /// At tick `now`: Windows' last input at `system`, the hook's last event at `hook`.
    pub fn check(&mut self, now: u32, system: u32, hook: u32) -> Verdict {
        if self.last_check.is_some_and(|t| now.wrapping_sub(t) < CHECK_EVERY_MS) {
            return Verdict::Fine;
        }
        self.last_check = Some(now);
        let fresh = self.last_system.is_some_and(|s| s != system);
        self.last_system = Some(system);
        let lag = system.wrapping_sub(hook) as i32;
        if lag <= LAG_MS as i32 {
            self.strikes = 0;
            return Verdict::Fine;
        }
        if fresh {
            self.strikes = self.strikes.saturating_add(1);
        }
        if self.strikes >= STRIKES && self.last_reinstall.is_none_or(|t| now.wrapping_sub(t) >= REINSTALL_EVERY_MS) {
            self.strikes = 0;
            self.last_reinstall = Some(now);
            return Verdict::Reinstall { lag_ms: lag as u32 };
        }
        Verdict::Fine
    }
}

/// The tick of the hook's last event (or of its installing), None before it runs.
#[cfg(windows)]
pub fn last_event_tick() -> Option<u32> {
    win::last_event_tick()
}

#[cfg(not(windows))]
pub fn last_event_tick() -> Option<u32> {
    None
}

/// Ask the hook's thread to take its hooks out and put them in again.
#[cfg(windows)]
pub fn reinstall() {
    win::reinstall();
}

#[cfg(not(windows))]
pub fn reinstall() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_in_a_hook_callback_is_caught_and_the_callback_keeps_working() {
        assert!(guarded(HookKind::Keyboard, || true));
        assert!(!guarded(HookKind::Keyboard, || false));
        assert!(!guarded(HookKind::Mouse, || panic!("a hook callback of this test panics")), "caught: the key is let through");
        // The callback's lock was held when it panicked: the next event still reaches it.
        let calls = std::sync::Mutex::new(0);
        assert!(!guarded(HookKind::Keyboard, || {
            let _held = calls.lock().unwrap();
            panic!("panics holding the lock");
        }));
        assert!(calls.is_poisoned());
        *lock_or_recover(&calls) += 1;
        assert_eq!(*lock_or_recover(&calls), 1);
    }

    /// A key or mouse event whose callback panics is passed on to the next hook, as one that is
    /// not taken; one that is taken is swallowed; and the callback runs marked as a hook's.
    #[test]
    fn an_event_whose_callback_panics_is_passed_on() {
        let passed = std::cell::Cell::new(0);
        let next = || {
            passed.set(passed.get() + 1);
            0
        };
        assert_eq!(answer(HookKind::Keyboard, true, || panic!("a keyboard callback of this test panics"), next), 0);
        assert_eq!(answer(HookKind::Mouse, true, || panic!("a mouse callback of this test panics"), next), 0);
        assert_eq!(passed.get(), 2, "both passed on");
        assert_eq!(answer(HookKind::Keyboard, true, || true, next), 1, "taken: swallowed");
        assert_eq!(answer(HookKind::Keyboard, false, || true, next), 0, "not an action: passed on, the callback not run");
        assert_eq!(passed.get(), 3);
        assert_eq!(answer(HookKind::Mouse, true, || crate::crash_log::current_hook() == Some(HookKind::Mouse), || 0), 1);
        assert_eq!(crate::crash_log::current_hook(), None);
    }

    /// The hook sees every input: nothing to do, however long the check runs.
    #[test]
    fn a_hook_that_sees_the_input_is_left_alone() {
        let mut l = Liveness::default();
        for s in 0..120u32 {
            let t = 10_000 + s * 1000;
            assert_eq!(l.check(t, t - 10, t - 5), Verdict::Fine);
        }
        // Nobody touches anything: Windows' input is old, the hook's too.
        let mut l = Liveness::default();
        for s in 0..120u32 {
            assert_eq!(l.check(100_000 + s * 1000, 5_000, 5_000), Verdict::Fine);
        }
    }

    /// The hook was removed: input goes on and the hook sees none. After three checks in a row,
    /// each with new input, it is put in again; again at most once a minute.
    #[test]
    fn a_hook_that_misses_new_input_on_three_checks_is_put_in_again_at_most_once_a_minute() {
        let mut l = Liveness::default();
        let hook = 1_000;
        let mut verdicts = Vec::new();
        for s in 0..5u32 {
            let t = 10_000 + s * 1000;
            verdicts.push(l.check(t, t - 100, hook));
        }
        assert_eq!(verdicts[..3], [Verdict::Fine, Verdict::Fine, Verdict::Fine], "the first check has nothing to compare with");
        assert_eq!(verdicts[3], Verdict::Reinstall { lag_ms: 12_900 - hook });
        assert_eq!(verdicts[4], Verdict::Fine);
        // Still nothing from the hook: not again before a minute has passed.
        for s in 5..63u32 {
            let t = 10_000 + s * 1000;
            assert_eq!(l.check(t, t - 100, hook), Verdict::Fine, "check {s}");
        }
        let t = 10_000 + 63 * 1000;
        assert!(matches!(l.check(t, t - 100, hook), Verdict::Reinstall { .. }), "a minute later");
        // Checks closer than a second apart are not counted.
        let mut l = Liveness::default();
        for i in 0..20u32 {
            let t = 10_000 + i * 100;
            assert_eq!(l.check(t, t - 50, hook), Verdict::Fine);
        }
    }

    /// Old input the hook missed once, with nothing new after it, is no reason; nor is input the
    /// hook sees in between.
    #[test]
    fn a_lag_without_new_input_or_broken_by_a_seen_event_puts_nothing_in() {
        let mut l = Liveness::default();
        for s in 0..10u32 {
            assert_eq!(l.check(20_000 + s * 1000, 15_000, 1_000), Verdict::Fine, "the same old input, check {s}");
        }
        let mut l = Liveness::default();
        for s in 0..10u32 {
            let t = 20_000 + s * 1000;
            let hook = if s % 2 == 0 { t - 50 } else { 1_000 };
            assert_eq!(l.check(t, t - 100, hook), Verdict::Fine, "check {s}");
        }
    }

    #[test]
    fn the_ticks_may_wrap() {
        let mut l = Liveness::default();
        let start = u32::MAX - 2_500;
        let mut got = Vec::new();
        for s in 0..5u32 {
            let t = start.wrapping_add(s * 1000);
            got.push(l.check(t, t.wrapping_sub(100), t.wrapping_sub(50)));
        }
        assert!(got.iter().all(|v| *v == Verdict::Fine), "seen input across the wrap: {got:?}");
    }
}

#[cfg(windows)]
mod win {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::HookKind;
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use std::sync::{Mutex, OnceLock, RwLock};

    use rdev::{Button, EventType, Key};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};

    use super::mods;
    use crate::hotkeys::Action;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    type Callback = Box<dyn FnMut(EventType) + Send>;
    static CALLBACK: OnceLock<Mutex<Callback>> = OnceLock::new();

    struct Taken {
        keys: Vec<super::TakenKey>,
        on_press: Box<dyn Fn(Action) -> bool + Send + Sync>,
    }
    static TAKEN: RwLock<Option<Taken>> = RwLock::new(None);
    /// The key whose press was taken, so its release is taken too (0: none).
    static TOOK_VK: AtomicU32 = AtomicU32::new(0);
    /// The tick of the last event either hook got, or of the hooks' installing (0: not yet).
    static LAST_EVENT: AtomicU32 = AtomicU32::new(0);
    /// The hooks' thread (0: not started).
    static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
    /// Asks the hooks' thread to put its hooks in again.
    const REHOOK: u32 = WM_APP + 0x31;

    pub fn last_event_tick() -> Option<u32> {
        let t = LAST_EVENT.load(Ordering::Relaxed);
        (t != 0).then_some(t)
    }

    pub fn reinstall() {
        let thread = HOOK_THREAD.load(Ordering::Relaxed);
        if thread != 0 {
            unsafe {
                let _ = PostThreadMessageW(thread, REHOOK, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn touch() {
        LAST_EVENT.store(unsafe { GetTickCount() }.max(1), Ordering::Relaxed);
    }

    pub fn set_taken_keys(keys: Vec<super::TakenKey>, on_press: Box<dyn Fn(Action) -> bool + Send + Sync>) {
        if let Ok(mut t) = TAKEN.write() {
            *t = Some(Taken { keys, on_press });
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

    /// Take a key event if it is one of the taken shortcuts.
    fn take(vk: u32, pressed: bool) -> bool {
        if !pressed {
            return TOOK_VK.compare_exchange(vk, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok();
        }
        let Ok(guard) = TAKEN.read() else { return false };
        let Some(t) = guard.as_ref() else { return false };
        if !t.keys.iter().any(|(_, k, _)| *k == vk) {
            return false;
        }
        let held = held_mods();
        match t.keys.iter().find(|(m, k, _)| *k == vk && *m == held) {
            Some((_, _, action)) if (t.on_press)(*action) => {
                TOOK_VK.store(vk, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    fn emit(ev: EventType) {
        if let Some(cb) = CALLBACK.get() {
            // A callback that panicked once (the panic was caught, see `guarded`) still gets
            // the events after it.
            (super::lock_or_recover(cb))(ev);
        }
    }

    /// The hooks are called by Windows: a panic must never unwind into it, which would end the
    /// process at once. Caught here, the panic is in `crash.log` as any other (the panic hook
    /// writes it), the event passes on to the other apps, and the hook keeps running.
    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let action = code == HC_ACTION as i32;
        let vk = if action {
            touch();
            unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode }
        } else {
            0
        };
        // 1: swallowed, no other app and not Windows sees this key.
        LRESULT(super::answer(HookKind::Keyboard, action, || keyboard_event(wparam.0 as u32, vk), || unsafe { CallNextHookEx(None, code, wparam, lparam) }.0))
    }

    /// One key event: true when it is taken (swallowed).
    fn keyboard_event(message: u32, vk: u32) -> bool {
        let pressed = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        if take(vk, pressed) {
            return true;
        }
        let key = vk_to_key(vk);
        match message {
            WM_KEYDOWN | WM_SYSKEYDOWN => emit(EventType::KeyPress(key)),
            WM_KEYUP | WM_SYSKEYUP => emit(EventType::KeyRelease(key)),
            _ => {}
        }
        false
    }

    unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let action = code == HC_ACTION as i32;
        let (x, y) = if action {
            touch();
            let pt = unsafe { (*(lparam.0 as *const MSLLHOOKSTRUCT)).pt };
            (pt.x, pt.y)
        } else {
            (0, 0)
        };
        LRESULT(super::answer(HookKind::Mouse, action, || mouse_event(wparam.0 as u32, x, y), || unsafe { CallNextHookEx(None, code, wparam, lparam) }.0))
    }

    /// One mouse event; never taken.
    fn mouse_event(message: u32, x: i32, y: i32) -> bool {
        let at = EventType::MouseMove { x: x as f64, y: y as f64 };
        let ev = match message {
            WM_MOUSEMOVE => Some(at),
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
                emit(at);
            }
            emit(ev);
        }
        false
    }

    pub fn listen(callback: Callback) -> Result<(), String> {
        CALLBACK.set(Mutex::new(callback)).map_err(|_| "the input hook is already running".to_string())?;
        unsafe {
            let module = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let install = || -> Result<(HHOOK, HHOOK), String> {
                let k = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module.into()), 0).map_err(|e| format!("keyboard hook: {e}"))?;
                let m = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module.into()), 0).map_err(|e| format!("mouse hook: {e}"))?;
                Ok((k, m))
            };
            let (mut keyboard, mut mouse) = install()?;
            touch();
            HOOK_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
            // Low-level hooks run inside this thread's message loop.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == REHOOK {
                    // Windows may have removed them already: taking them out then does nothing.
                    let _ = UnhookWindowsHookEx(keyboard);
                    let _ = UnhookWindowsHookEx(mouse);
                    match install() {
                        Ok((k, m)) => (keyboard, mouse) = (k, m),
                        Err(e) => eprintln!("input hook not put in again: {e}"),
                    }
                    touch();
                }
            }
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
