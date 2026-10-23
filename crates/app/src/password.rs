//! Keeps passwords out of the key overlay.
//!
//! While a recording logs keys, a [`PasswordWatch`] follows keyboard focus and
//! asks UI Automation whether the focused control is a password field
//! (Windows password boxes and browser `<input type=password>` both say so).
//! It runs only while keys are logged, because a UI Automation client makes
//! browsers build their accessibility trees.
//!
//! It fails closed: from a focus change until the answer arrives, and whenever
//! the answer can't be had, keys count as going into a password field.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct PasswordWatch {
    blocked: Arc<AtomicBool>,
    #[cfg(windows)]
    stop: Arc<win::Stop>,
}

impl PasswordWatch {
    pub fn start() -> Self {
        let blocked = Arc::new(AtomicBool::new(true));
        #[cfg(windows)]
        {
            let stop = Arc::new(win::Stop::default());
            let (b, st) = (blocked.clone(), stop.clone());
            std::thread::spawn(move || {
                if let Err(e) = win::run(&b, &st) {
                    eprintln!("password field check unavailable, not logging keys: {e}");
                    b.store(true, Ordering::Relaxed);
                }
            });
            Self { blocked, stop }
        }
        #[cfg(not(windows))]
        {
            // No check on other systems yet.
            blocked.store(false, Ordering::Relaxed);
            Self { blocked }
        }
    }

    /// Whether key presses should be left out right now.
    pub fn blocked(&self) -> bool {
        self.blocked.load(Ordering::Relaxed)
    }
}

#[cfg(windows)]
impl Drop for PasswordWatch {
    fn drop(&mut self) {
        self.stop.stop();
    }
}

#[cfg(windows)]
mod win {
    use std::cell::{Cell, RefCell};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Accessibility::{CUIAutomation, HWINEVENTHOOK, IUIAutomation, SetWinEventHook, UnhookWinEvent};
    use windows::Win32::UI::WindowsAndMessaging::{
        EVENT_OBJECT_FOCUS, GetMessageW, MSG, PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW, WINEVENT_OUTOFCONTEXT, WM_APP, WM_QUIT,
    };

    const WM_FOCUS_CHANGED: u32 = WM_APP + 1;

    /// Ends the watch thread, even if it has not got going yet.
    #[derive(Default)]
    pub struct Stop {
        stopped: AtomicBool,
        thread_id: AtomicU32,
    }

    impl Stop {
        pub fn stop(&self) {
            self.stopped.store(true, Ordering::SeqCst);
            let id = self.thread_id.load(Ordering::SeqCst);
            if id != 0 {
                let _ = unsafe { PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            }
        }
    }

    // The focus callback runs on the watch thread, inside its message loop.
    thread_local! {
        static BLOCKED: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
        static FOCUS_MOVED: Cell<bool> = const { Cell::new(false) };
    }

    unsafe extern "system" fn on_focus(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
        BLOCKED.with_borrow(|b| b.as_ref().map(|b| b.store(true, Ordering::Relaxed)));
        FOCUS_MOVED.set(true);
        // Ask outside this callback: UI Automation calls from inside it can re-enter.
        let _ = unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_FOCUS_CHANGED, WPARAM(0), LPARAM(0)) };
    }

    pub fn run(blocked: &Arc<AtomicBool>, stop: &Stop) -> Result<(), String> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().map_err(|e| e.to_string())?;
            let result = watch(blocked, stop);
            CoUninitialize();
            result
        }
    }

    unsafe fn watch(blocked: &Arc<AtomicBool>, stop: &Stop) -> Result<(), String> {
        let automation: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.map_err(|e| e.to_string())?;
        BLOCKED.set(Some(blocked.clone()));
        let hook = unsafe { SetWinEventHook(EVENT_OBJECT_FOCUS, EVENT_OBJECT_FOCUS, None, Some(on_focus), 0, 0, WINEVENT_OUTOFCONTEXT) };
        if hook.is_invalid() {
            return Err("could not watch focus changes".into());
        }
        // Make the message queue before publishing the thread id, so WM_QUIT can be posted.
        let mut msg = MSG::default();
        unsafe {
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        }
        stop.thread_id.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);

        let check = || {
            FOCUS_MOVED.set(false);
            let password = unsafe { automation.GetFocusedElement().and_then(|e| e.CurrentIsPassword()) }.map(|b| b.as_bool()).unwrap_or(true);
            // Focus may have moved again while we asked; that check will follow.
            if !FOCUS_MOVED.get() {
                blocked.store(password, Ordering::Relaxed);
            }
        };
        if !stop.stopped.load(Ordering::SeqCst) {
            check();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                if msg.message == WM_FOCUS_CHANGED {
                    // One check covers a burst of focus changes.
                    while unsafe { PeekMessageW(&mut msg, None, WM_FOCUS_CHANGED, WM_FOCUS_CHANGED, PM_REMOVE) }.as_bool() {}
                    check();
                }
            }
        }
        unsafe {
            let _ = UnhookWinEvent(hook);
        }
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::time::{Duration, Instant};

    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, ES_PASSWORD, MSG, PM_REMOVE, PeekMessageW, SW_SHOW, SetForegroundWindow, ShowWindow,
        TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows::core::w;

    use super::PasswordWatch;

    /// Pump this thread's messages until `want` or a timeout.
    fn wait_for(watch: &PasswordWatch, want: bool) -> bool {
        let end = Instant::now() + Duration::from_secs(3);
        let mut msg = MSG::default();
        while Instant::now() < end {
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if watch.blocked() == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Opens a window with a text box and a password box and moves focus
    /// between them, so it takes focus for a second.
    #[test]
    #[ignore = "takes keyboard focus; run by hand with --ignored"]
    fn password_box_blocks_keys() {
        unsafe {
            let window = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("Mythicshot password test"), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 200, 200, 360, 160, None, None, None, None).unwrap();
            let text = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), WS_CHILD | WS_VISIBLE, 10, 10, 300, 24, Some(window), None, None, None).unwrap();
            let pass = CreateWindowExW(WINDOW_EX_STYLE(0), w!("EDIT"), w!(""), WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_PASSWORD as u32), 10, 50, 300, 24, Some(window), None, None, None).unwrap();
            let _ = ShowWindow(window, SW_SHOW);
            let _ = SetForegroundWindow(window);

            let _ = SetFocus(Some(text));
            let watch = PasswordWatch::start();
            let in_text = wait_for(&watch, false);
            let _ = SetFocus(Some(pass));
            let in_pass = wait_for(&watch, true);
            let _ = SetFocus(Some(text));
            let back = wait_for(&watch, false);
            drop(watch);
            let _ = DestroyWindow(window);
            assert!(in_text, "plain text box should log keys");
            assert!(in_pass, "password box should block keys");
            assert!(back, "leaving the password box should log keys again");
        }
    }
}
