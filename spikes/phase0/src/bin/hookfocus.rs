//! Test helper: does a low-level keyboard hook still see keys while a window
//! of its own process has focus? Hooks on a second thread, opens a plain
//! window on the main thread, and logs every key the hook sees for N seconds
//! (default 8). Prints the window's position so a test can click it.

#[cfg(windows)]
fn main() {
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
        SetWindowsHookExW, TranslateMessage, WH_KEYBOARD_LL, WINDOW_EX_STYLE, WM_KEYDOWN, WM_SYSKEYDOWN, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows::core::w;

    unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            println!("hook vk={:#x} down={down}", info.vkCode);
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if m == WM_KEYDOWN {
            println!("window vk={:#x}", w.0);
        }
        unsafe { DefWindowProcW(h, m, w, l) }
    }

    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(8);
    std::thread::spawn(|| unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0).expect("hook");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
    });
    unsafe {
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), lpszClassName: w!("HookFocusTest"), ..Default::default() };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("HookFocusTest"), w!("Hook focus test"), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 300, 300, 400, 250, None, None, None, None).expect("window");
        println!("window at 300,300 size 400x250");
        // `raw` as the second argument: register raw mouse and keyboard input
        // the way winit does when its window has focus.
        if std::env::args().nth(2).as_deref() == Some("raw") {
            use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_DEVNOTIFY, RegisterRawInputDevices};
            let devs = [
                RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_DEVNOTIFY, hwndTarget: hwnd },
                RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_DEVNOTIFY, hwndTarget: hwnd },
            ];
            let ok = RegisterRawInputDevices(&devs, std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_ok();
            println!("raw input registered: {ok}");
        }
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut msg = MSG::default();
        while std::time::Instant::now() < end {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(not(windows))]
fn main() {}
