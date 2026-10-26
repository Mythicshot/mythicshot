//! Test helper: a bare low-level keyboard hook that swallows F15 and logs every
//! key it sees, to check whether swallowing a key stops later keys arriving.
//! Runs for the number of seconds given (default 6).

#[cfg(windows)]
fn main() {
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_REMOVE, PeekMessageW, SetWindowsHookExW, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    };

    unsafe extern "system" fn proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            println!("key vk={:#x} down={down}", info.vkCode);
            if info.vkCode == 0x7E {
                println!("  swallowed");
                return LRESULT(1);
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(6);
    unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), None, 0).expect("hook");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut msg = MSG::default();
        while std::time::Instant::now() < end {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(not(windows))]
fn main() {}
