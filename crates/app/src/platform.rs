//! Small OS-specific helpers. Each has a Windows version and a fallback.

use std::path::Path;

/// True if this is the only running Mythicshot. Holds a named mutex for the
/// life of the process on Windows.
#[cfg(windows)]
pub fn single_instance() -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::w;
    unsafe {
        match CreateMutexW(None, true, w!("Local\\Mythicshot-single-instance")) {
            // The handle is never closed, so the mutex lives as long as the process.
            Ok(_handle) => GetLastError() != ERROR_ALREADY_EXISTS,
            Err(_) => true,
        }
    }
}

#[cfg(not(windows))]
pub fn single_instance() -> bool {
    true
}

/// Whether Windows' "Use the Print screen key to open screen capture" setting
/// is on. `None` when the setting has never been written.
#[cfg(windows)]
pub fn snipping_tool_owns_prtsc() -> Option<bool> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Keyboard"),
            w!("PrintScreenKeyForSnippingEnabled"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(value != 0)
}

#[cfg(not(windows))]
pub fn snipping_tool_owns_prtsc() -> Option<bool> {
    Some(false)
}

/// Stop receiving raw keyboard input, which the window library asks Windows
/// for at start-up. While a process gets raw keyboard input, Windows stops
/// calling its low-level keyboard hook whenever one of its own windows has
/// focus, so Print Screen and the recording shortcuts would go dead while a
/// Mythicshot window is in front. egui doesn't use raw keyboard input.
#[cfg(windows)]
pub fn stop_raw_keyboard_input() {
    use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_REMOVE, RegisterRawInputDevices};
    let keyboard = [RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_REMOVE, hwndTarget: Default::default() }];
    if let Err(e) = unsafe { RegisterRawInputDevices(&keyboard, std::mem::size_of::<RAWINPUTDEVICE>() as u32) } {
        eprintln!("could not stop raw keyboard input: {e}");
    }
}

#[cfg(not(windows))]
pub fn stop_raw_keyboard_input() {}

/// Turn Windows' "Use the Print screen key to open screen capture" on or off,
/// and tell running apps the keyboard settings changed.
#[cfg(windows)]
pub fn set_snipping_tool_prtsc(on: bool) -> Result<(), String> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, RegSetKeyValueW};
    use windows::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};
    use windows::core::w;
    let value: u32 = on.into();
    unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, w!("Control Panel\\Keyboard"), w!("PrintScreenKeyForSnippingEnabled"), REG_DWORD.0, Some(&value as *const u32 as *const _), 4)
            .ok()
            .map_err(|e| e.to_string())?;
        let area = w!("Keyboard");
        let _ = SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(area.as_ptr() as isize), SMTO_ABORTIFHUNG, 2000, None);
    }
    Ok(())
}

/// Steps the Windows installer runs with `mythicshot.exe --installer <step>`:
/// `prtsc-off` gives the Print Screen key to Mythicshot during install;
/// `prtsc-ask` asks, while uninstalling, whether to give it back to Snipping Tool.
#[cfg(windows)]
pub fn installer_step(step: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MessageBoxW};
    use windows::core::{HSTRING, w};
    match step {
        "prtsc-off" => {
            if snipping_tool_owns_prtsc() != Some(false) {
                let _ = set_snipping_tool_prtsc(false);
            }
        }
        "prtsc-ask" => {
            // Only worth asking if the key is not already Snipping Tool's.
            if snipping_tool_owns_prtsc() == Some(false) {
                let text = HSTRING::from(
                    "Mythicshot is being removed.\n\nTurn Snipping Tool's Print Screen shortcut back on?\n\nYou can change this later in Settings › Accessibility › Keyboard.",
                );
                let answer = unsafe { MessageBoxW(None, &text, w!("Mythicshot Setup"), MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND) };
                if answer == IDYES {
                    let _ = set_snipping_tool_prtsc(true);
                }
            }
        }
        _ => {}
    }
}

#[cfg(not(windows))]
pub fn installer_step(_step: &str) {}

/// Move the mouse pointer by a few pixels (arrow keys in the region selector).
#[cfg(windows)]
pub fn nudge_cursor(dx: i32, dy: i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
    let mut p = POINT::default();
    unsafe {
        if GetCursorPos(&mut p).is_ok() {
            let _ = SetCursorPos(p.x + dx, p.y + dy);
        }
    }
}

#[cfg(not(windows))]
pub fn nudge_cursor(_dx: i32, _dy: i32) {}

/// Mouse pointer position in desktop pixels.
#[cfg(windows)]
pub fn cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| (p.x, p.y)) }
}

#[cfg(not(windows))]
pub fn cursor_pos() -> Option<(i32, i32)> {
    None
}

/// A native window handle as a plain number, so it can cross threads.
pub type WindowId = isize;

/// A window's outer and client rectangles in desktop pixels.
#[cfg(windows)]
pub fn window_rects(hwnd: WindowId) -> Option<(mythicshot_capture::PxRect, mythicshot_capture::PxRect)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowInfo, WINDOWINFO};
    let mut info = WINDOWINFO { cbSize: std::mem::size_of::<WINDOWINFO>() as u32, ..Default::default() };
    unsafe { GetWindowInfo(HWND(hwnd as *mut _), &mut info).ok()? };
    let px = |r: RECT| mythicshot_capture::PxRect::new(r.left, r.top, (r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32);
    Some((px(info.rcWindow), px(info.rcClient)))
}

/// The DPI Windows has given a window (96 is 100%).
#[cfg(windows)]
pub fn window_dpi(hwnd: WindowId) -> Option<u32> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    let dpi = unsafe { GetDpiForWindow(HWND(hwnd as *mut _)) };
    (dpi != 0).then_some(dpi)
}

/// Move and size a window to an outer rectangle in desktop pixels, without showing it,
/// activating it or changing its place in the stack.
#[cfg(windows)]
pub fn place_window(hwnd: WindowId, outer: mythicshot_capture::PxRect) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos};
    unsafe { SetWindowPos(HWND(hwnd as *mut _), None, outer.x, outer.y, outer.w as i32, outer.h as i32, SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER).is_ok() }
}

#[cfg(windows)]
pub fn is_window_visible(hwnd: WindowId) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    unsafe { IsWindowVisible(HWND(hwnd as *mut _)).as_bool() }
}

/// Bring one of our windows to the front with a plain request, without the synthetic Alt key
/// press winit's focus request sends. False when Windows did not allow it.
#[cfg(windows)]
pub fn take_foreground(hwnd: WindowId) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    unsafe {
        let h = HWND(hwnd as *mut _);
        let _ = SetForegroundWindow(h);
        GetForegroundWindow() == h
    }
}

/// Cloak or uncloak a window: a cloaked window is shown but never composited, so it can be
/// shown and painted once at its final size before anyone sees it.
#[cfg(windows)]
pub fn set_cloaked(hwnd: WindowId, cloaked: bool) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
    let value = windows::core::BOOL::from(cloaked);
    unsafe { DwmSetWindowAttribute(HWND(hwnd as *mut _), DWMWA_CLOAK, &value as *const _ as *const _, std::mem::size_of_val(&value) as u32).is_ok() }
}

/// What Windows says about how a window is composited, for `capture.log`: its extended style,
/// whether it is a layered window and if so whether its layered attributes were ever set (a
/// layered window without them is not drawn on screen), and whether it is cloaked.
#[cfg(windows)]
pub fn composition_note(hwnd: WindowId) -> String {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, GetLayeredWindowAttributes, GetWindowLongPtrW, WS_EX_LAYERED};
    let h = HWND(hwnd as *mut _);
    let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
    let layered = if ex & WS_EX_LAYERED.0 == 0 {
        "not layered"
    } else if unsafe { GetLayeredWindowAttributes(h, None, None, None) }.is_ok() {
        "layered, attributes set"
    } else {
        "layered, attributes NOT set"
    };
    let mut cloaked = 0u32;
    let read = unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, std::mem::size_of_val(&cloaked) as u32) }.is_ok();
    let cloak = if read { cloaked.to_string() } else { "?".to_string() };
    format!("exstyle {ex:#x} · {layered} · cloaked {cloak}")
}

#[cfg(not(windows))]
pub fn composition_note(_hwnd: WindowId) -> String {
    String::new()
}

/// One of this process's hidden top-level windows with this exact title: a window egui has just
/// created hidden, to be placed before it is shown.
#[cfg(windows)]
pub fn find_own_hidden_window(title: &str) -> Option<WindowId> {
    find_own(title, true)
}

/// This process's first top-level window with this exact title, hidden ones only if asked.
#[cfg(windows)]
fn find_own(title: &str, hidden_only: bool) -> Option<WindowId> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId, IsWindowVisible};
    use windows::core::PCWSTR;
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let mut after: Option<HWND> = None;
    // A bound on the windows looked at, in case the list changes while it is walked.
    for _ in 0..64 {
        let h = unsafe { FindWindowExW(None, after, PCWSTR::null(), PCWSTR(wide.as_ptr())) }.ok()?;
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid == unsafe { GetCurrentProcessId() } && !(hidden_only && unsafe { IsWindowVisible(h) }.as_bool()) {
            return Some(h.0 as WindowId);
        }
        after = Some(h);
    }
    None
}

#[cfg(not(windows))]
pub fn find_own_hidden_window(_title: &str) -> Option<WindowId> {
    None
}

#[cfg(not(windows))]
pub fn set_cloaked(_hwnd: WindowId, _cloaked: bool) -> bool {
    false
}

#[cfg(not(windows))]
pub fn window_rects(_hwnd: WindowId) -> Option<(mythicshot_capture::PxRect, mythicshot_capture::PxRect)> {
    None
}

#[cfg(not(windows))]
pub fn window_dpi(_hwnd: WindowId) -> Option<u32> {
    None
}

#[cfg(not(windows))]
pub fn place_window(_hwnd: WindowId, _outer: mythicshot_capture::PxRect) -> bool {
    false
}

#[cfg(not(windows))]
pub fn is_window_visible(_hwnd: WindowId) -> bool {
    false
}

#[cfg(not(windows))]
pub fn take_foreground(_hwnd: WindowId) -> bool {
    false
}

/// Keep a window out of every screen capture, ours and other apps' (OBS
/// included), so the recording controls never appear in a recording.
#[cfg(windows)]
pub fn exclude_from_capture(hwnd: WindowId) -> bool {
    use windows::Win32::Foundation::HWND;
    // Debug builds can leave our windows visible to capture, to screenshot them in tests.
    #[cfg(debug_assertions)]
    if std::env::var_os("MYTHICSHOT_TEST_SHOW_IN_CAPTURE").is_some() {
        return true;
    }
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};
    unsafe { SetWindowDisplayAffinity(HWND(hwnd as *mut _), WDA_EXCLUDEFROMCAPTURE).is_ok() }
}

/// Find one of our windows by its exact title: only this process's, never another program's
/// window that happens to have the same title.
#[cfg(windows)]
pub fn find_window(title: &str) -> Option<WindowId> {
    find_own(title, false)
}

#[cfg(windows)]
pub fn foreground_window() -> Option<WindowId> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let h = unsafe { GetForegroundWindow() };
    (!h.0.is_null()).then_some(h.0 as WindowId)
}

/// Give focus back to the app that had it before one of our windows appeared.
#[cfg(windows)]
pub fn set_foreground(hwnd: WindowId) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    unsafe {
        let _ = SetForegroundWindow(HWND(hwnd as *mut _));
    }
}

/// Whose window it is, for remembering which app the user was in before the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    /// One of this app's own windows.
    Ours,
    /// The taskbar, the tray's overflow or the desktop: what has focus while the tray icon is clicked.
    Shell,
    Other,
}

/// The window classes of the taskbar and the tray's overflow panel, old and new.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_shell_class(class: &str) -> bool {
    matches!(class, "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "NotifyIconOverflowWindow" | "TopLevelWindowForOverflowXamlIsland" | "Progman" | "WorkerW")
}

#[cfg(windows)]
pub fn window_kind(hwnd: WindowId) -> WindowKind {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetShellWindow, GetWindowThreadProcessId};
    let h = HWND(hwnd as *mut _);
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return WindowKind::Ours;
        }
        let mut name = [0u16; 128];
        let len = GetClassNameW(h, &mut name).max(0) as usize;
        if h == GetShellWindow() || is_shell_class(&String::from_utf16_lossy(&name[..len])) {
            return WindowKind::Shell;
        }
    }
    WindowKind::Other
}

#[cfg(not(windows))]
pub fn window_kind(_hwnd: WindowId) -> WindowKind {
    WindowKind::Other
}

#[cfg(not(windows))]
pub fn exclude_from_capture(_hwnd: WindowId) -> bool {
    false
}

#[cfg(not(windows))]
pub fn find_window(_title: &str) -> Option<WindowId> {
    None
}

#[cfg(not(windows))]
pub fn foreground_window() -> Option<WindowId> {
    None
}

#[cfg(not(windows))]
pub fn set_foreground(_hwnd: WindowId) {}

#[cfg(windows)]
const RUN_KEY: windows::core::PCWSTR = windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

/// Whether Mythicshot starts when the user signs in (the per-user Run key).
#[cfg(windows)]
pub fn autostart_enabled() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Mythicshot"), RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok() }
}

#[cfg(windows)]
pub fn set_autostart(on: bool) -> Result<(), String> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW};
    use windows::core::w;
    unsafe {
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Mythicshot"), REG_SZ.0, Some(value.as_ptr() as *const _), (value.len() * 2) as u32)
                .ok()
                .map_err(|e| format!("could not turn on start with Windows: {e}"))
        } else {
            let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Mythicshot"));
            if r.is_ok() || r.0 == 2 { Ok(()) } else { Err(format!("could not turn off start with Windows: {}", r.0)) }
        }
    }
}

#[cfg(not(windows))]
pub fn autostart_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_autostart(_on: bool) -> Result<(), String> {
    Err("Start at login is not available on this system yet".into())
}

/// Explorer's argument to open a file with its default app: the path in quotes, so commas, spaces and any
/// other characters in it stay part of the name (a Windows path cannot hold a quote). It must reach
/// Explorer as it is, not re-quoted as a whole.
#[cfg_attr(not(windows), allow(dead_code))]
fn open_arg(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

/// Open a file with its default app.
pub fn open_path(path: &Path) {
    #[cfg(windows)]
    let result = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer").raw_arg(open_arg(path)).spawn()
    };
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = result {
        eprintln!("could not open {}: {e}", path.display());
    }
}

/// Explorer's argument to open a folder with `path` selected. The path is quoted, so commas, spaces and
/// any other characters in it stay part of the name (a Windows path cannot hold a quote). It must reach
/// Explorer as it is, not re-quoted as a whole.
#[cfg_attr(not(windows), allow(dead_code))]
fn select_arg(path: &Path) -> String {
    format!("/select,\"{}\"", path.display())
}

/// Show a file selected in the file manager.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    let result = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer").raw_arg(select_arg(path)).spawn()
    };
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    if let Err(e) = result {
        eprintln!("could not show {}: {e}", path.display());
    }
}

pub fn open_folder(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    if let Err(e) = std::process::Command::new(cmd).arg(path).spawn() {
        eprintln!("could not open {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_taskbar_and_the_desktop_are_shell_windows() {
        for class in ["Shell_TrayWnd", "Shell_SecondaryTrayWnd", "NotifyIconOverflowWindow", "TopLevelWindowForOverflowXamlIsland", "Progman", "WorkerW"] {
            assert!(is_shell_class(class), "{class}");
        }
        for class in ["Chrome_WidgetWin_1", "Notepad", "CabinetWClass", ""] {
            assert!(!is_shell_class(class), "{class}");
        }
    }

    #[test]
    fn the_file_to_select_is_quoted_whole() {
        assert_eq!(select_arg(Path::new(r"C:\Users\me\Tours\Billing, v2 tour.html")), r#"/select,"C:\Users\me\Tours\Billing, v2 tour.html""#);
        assert_eq!(select_arg(Path::new(r"D:\Führung\演示.html")), r#"/select,"D:\Führung\演示.html""#);
    }

    #[test]
    fn the_file_to_open_is_quoted_whole() {
        assert_eq!(open_arg(Path::new(r"C:\Users\me\Raid guides, v2\index.html")), r#""C:\Users\me\Raid guides, v2\index.html""#);
        assert_eq!(open_arg(Path::new(r"D:\Führung & Co\演示.html")), r#""D:\Führung & Co\演示.html""#);
    }
}
