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

/// Find one of our windows by its exact title.
#[cfg(windows)]
pub fn find_window(title: &str) -> Option<WindowId> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    use windows::core::PCWSTR;
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())).ok().map(|h| h.0 as WindowId) }
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

/// Open a file with its default app.
pub fn open_path(path: &Path) {
    #[cfg(windows)]
    let result = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = result {
        eprintln!("could not open {}: {e}", path.display());
    }
}

/// Show a file selected in the file manager.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    let result = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
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
