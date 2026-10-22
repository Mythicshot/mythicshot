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
