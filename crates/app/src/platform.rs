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
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("set snipping tool print screen {on}")) {
        return Ok(());
    }
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
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("nudge cursor {dx} {dy}")) {
        return;
    }
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

/// The monitors as Windows' window manager knows them, cheaply: each one's rectangle in desktop
/// pixels and its effective DPI, from `EnumDisplayMonitors`, `GetMonitorInfoW` and
/// `GetDpiForMonitor` only. Unlike the full monitor listing it asks the display driver nothing
/// (no display settings) and loads no library, so it can be read while a selection is dragged.
/// The rectangle and DPI are the ones the full listing reports for a process aware of each
/// monitor's DPI, as this one is.
#[cfg(windows)]
pub fn monitor_layout() -> Vec<(mythicshot_capture::PxRect, u32)> {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::core::BOOL {
        let out = unsafe { &mut *(data.0 as *mut Vec<(mythicshot_capture::PxRect, u32)>) };
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (mut dx, mut dy) = (0u32, 0u32);
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() && unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_ok() {
            let r = info.rcMonitor;
            out.push((mythicshot_capture::PxRect::new(r.left, r.top, (r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32), dx));
        }
        true.into()
    }
    let mut out: Vec<(mythicshot_capture::PxRect, u32)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// The primary monitor's work area (the monitor less the taskbar and docked bars) in desktop
/// pixels, and its effective DPI, from `MonitorFromPoint`, `GetMonitorInfoW` and
/// `GetDpiForMonitor`, as [`monitor_layout`] reads them. The primary monitor is where Windows
/// creates a new window that is given no monitor of its own, as the main window is.
#[cfg(windows)]
pub fn primary_work_area() -> Option<(mythicshot_capture::PxRect, u32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let (mut dx, mut dy) = (0u32, 0u32);
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() || unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_err() || dx == 0 {
        return None;
    }
    let r = info.rcWork;
    Some((mythicshot_capture::PxRect::new(r.left, r.top, (r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32), dx))
}

#[cfg(not(windows))]
pub fn primary_work_area() -> Option<(mythicshot_capture::PxRect, u32)> {
    None
}

/// The client size to take as a window's size, from what Windows says about it, or None when the
/// window is not in its normal state: maximized or minimized (`show_cmd` is not
/// `SW_SHOWNORMAL`, or `iconic`), or snapped or otherwise arranged (its window rectangle is not
/// the size of its restore rectangle, `rcNormalPosition`). In the normal state the restore
/// rectangle is the window rectangle, at the window's current DPI, so its client size is its
/// size; in any other state nothing is taken.
pub fn normal_state_client(show_cmd: u32, iconic: bool, window: (i32, i32), restore: (i32, i32), client: (i32, i32)) -> Option<(i32, i32)> {
    const SW_SHOWNORMAL: u32 = 1;
    (show_cmd == SW_SHOWNORMAL && !iconic && window == restore && client.0 > 0 && client.1 > 0).then_some(client)
}

/// A window's client size in pixels and its DPI, when it is in its normal state (see
/// [`normal_state_client`]); None when it is maximized, minimized, snapped, or cannot be read.
#[cfg(windows)]
pub fn normal_client_px(hwnd: WindowId) -> Option<(i32, i32, u32)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowPlacement, GetWindowRect, IsIconic, WINDOWPLACEMENT};
    let h = HWND(hwnd as *mut _);
    let mut placement = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(h, &mut placement) }.ok()?;
    let (mut window, mut client) = (RECT::default(), RECT::default());
    unsafe { GetWindowRect(h, &mut window) }.ok()?;
    unsafe { GetClientRect(h, &mut client) }.ok()?;
    let dpi = unsafe { GetDpiForWindow(h) };
    if dpi == 0 {
        return None;
    }
    let size = |r: RECT| (r.right - r.left, r.bottom - r.top);
    let iconic = unsafe { IsIconic(h) }.as_bool();
    let (w, h) = normal_state_client(placement.showCmd, iconic, size(window), size(placement.rcNormalPosition), size(client))?;
    Some((w, h, dpi))
}

#[cfg(not(windows))]
pub fn normal_client_px(_hwnd: WindowId) -> Option<(i32, i32, u32)> {
    None
}

/// Whether `hwnd` is still a window of this process with exactly this title: a handle kept from
/// earlier may have gone, and Windows may have given its number to another window since.
#[cfg(windows)]
pub fn is_own_window_titled(hwnd: WindowId, title: &str) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, GetWindowThreadProcessId};
    let h = HWND(hwnd as *mut _);
    let mut pid = 0u32;
    if unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) } == 0 || pid != unsafe { GetCurrentProcessId() } {
        return false;
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    n >= 0 && String::from_utf16_lossy(&buf[..n as usize]) == title
}

#[cfg(not(windows))]
pub fn is_own_window_titled(_hwnd: WindowId, _title: &str) -> bool {
    false
}

/// This process's visible top-level window with this exact title whose client area is `client`
/// pixels, give or take `tolerance`: one window of several with the same title (the app's
/// hidden root window has it too, and an editor may), told apart by being shown and by its size.
#[cfg(windows)]
pub fn find_visible_own_window(title: &str, client: (i32, i32), tolerance: i32) -> Option<WindowId> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetClientRect, GetWindowThreadProcessId, IsWindowVisible};
    use windows::core::PCWSTR;
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let mut after: Option<HWND> = None;
    // A bound on the windows looked at, in case the list changes while it is walked.
    for _ in 0..64 {
        let h = unsafe { FindWindowExW(None, after, PCWSTR::null(), PCWSTR(wide.as_ptr())) }.ok()?;
        after = Some(h);
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid != unsafe { GetCurrentProcessId() } || !unsafe { IsWindowVisible(h) }.as_bool() {
            continue;
        }
        let mut r = RECT::default();
        if unsafe { GetClientRect(h, &mut r) }.is_ok() && ((r.right - r.left) - client.0).abs() <= tolerance && ((r.bottom - r.top) - client.1).abs() <= tolerance {
            return Some(h.0 as WindowId);
        }
    }
    None
}

#[cfg(not(windows))]
pub fn find_visible_own_window(_title: &str, _client: (i32, i32), _tolerance: i32) -> Option<WindowId> {
    None
}

#[cfg(not(windows))]
pub fn monitor_layout() -> Vec<(mythicshot_capture::PxRect, u32)> {
    mythicshot_capture::list_monitors().iter().map(|m| (m.rect, (m.scale * 96.0).round() as u32)).collect()
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
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("take foreground {hwnd}")) {
        return false;
    }
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

/// Keep a window out of every screen capture, ours and every other app's
/// (screen recorders included), so the recording controls never appear in a recording.
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

/// One strip of the glass band to open: where it goes, in desktop pixels, and its pixels, top
/// row first, premultiplied BGRA.
pub struct StripPixels {
    pub rect: mythicshot_capture::PxRect,
    pub pixels: Vec<u8>,
}

/// What opening one strip came to, for `capture.log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedStrip {
    /// The window, if it was made.
    pub window: Option<WindowId>,
    /// How Windows composites it ([`composition_note`]), or why it was not made.
    pub note: String,
    /// `UpdateLayeredWindow` took its pixels.
    pub pixels_set: bool,
    /// It is kept out of every capture.
    pub excluded: bool,
    pub visible: bool,
}

/// The glass band's open windows and the thread that made them (only that thread may destroy them).
#[derive(Debug)]
pub struct BandWindows {
    windows: Vec<WindowId>,
    thread: u32,
}

/// Open the glass band's strips: one native layered window each, made on this thread, which must
/// keep its message loop running (the app's main thread does). See [`band_native::open_strip`].
/// The windows made, if any, and what became of each strip, in order.
pub fn band_open(strips: &[StripPixels]) -> (Option<BandWindows>, Vec<OpenedStrip>) {
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("band open {}", strips.iter().map(|s| mythicshot_capture_px(s.rect)).collect::<Vec<_>>().join("; "))) {
        let opened: Vec<OpenedStrip> = (1..=strips.len()).map(|i| OpenedStrip { window: Some(i as WindowId), note: "stub".into(), pixels_set: true, excluded: true, visible: true }).collect();
        return (Some(BandWindows { windows: (1..=strips.len() as WindowId).collect(), thread: 0 }), opened);
    }
    #[cfg(windows)]
    {
        let opened: Vec<OpenedStrip> = strips.iter().map(|s| band_native::open_strip(s)).collect();
        let windows: Vec<WindowId> = opened.iter().filter_map(|o| o.window).collect();
        let thread = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        ((!windows.is_empty()).then_some(BandWindows { windows, thread }), opened)
    }
    #[cfg(not(windows))]
    {
        let opened = strips.iter().map(|_| OpenedStrip { window: None, note: "not on this system".into(), pixels_set: false, excluded: false, visible: false }).collect();
        (None, opened)
    }
}

#[cfg(test)]
fn mythicshot_capture_px(r: mythicshot_capture::PxRect) -> String {
    crate::capture_log::px(r)
}

/// Put the band's strips back on top of every other topmost window, shown, without activating
/// them or moving them: another topmost window (or one that became topmost later) can otherwise
/// cover them.
pub fn band_raise(band: &BandWindows) {
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("band raise {}", band.windows.len())) {
        return;
    }
    #[cfg(windows)]
    for &h in &band.windows {
        band_native::raise(h);
    }
    #[cfg(not(windows))]
    let _ = band;
}

/// Destroy the band's strips. From another thread than the one that made them, each is asked to
/// close instead, and its own thread destroys it.
pub fn band_close(band: BandWindows) {
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("band close {}", band.windows.len())) {
        return;
    }
    #[cfg(windows)]
    {
        let here = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        for &h in &band.windows {
            band_native::close(h, here == band.thread);
        }
    }
    #[cfg(not(windows))]
    let _ = band;
}

/// The glass band's strips as native windows. Each is made layered from the start, given its
/// pixels with `UpdateLayeredWindow` while still hidden, and only then shown, topmost and without
/// activation. Windows keeps those pixels and composites them itself: nothing is painted later,
/// so the window cannot be shown blank the way a window made layered while hidden, and painted by
/// the window library afterwards, was.
#[cfg(windows)]
mod band_native {
    use super::{OpenedStrip, StripPixels, WindowId};
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HGDIOBJ, ReleaseDC, SelectObject,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, HTTRANSPARENT, HWND_TOPMOST, IsWindowVisible, MA_NOACTIVATE, PostMessageW, RegisterClassExW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_SHOWWINDOW,
        SetWindowPos, ULW_ALPHA, UpdateLayeredWindow, WM_CLOSE, WM_MOUSEACTIVATE, WM_NCHITTEST, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };
    use windows::core::{PCWSTR, w};

    const CLASS: PCWSTR = w!("MythicshotGlassBand");

    /// Every hit passes through to the window below, and a click never activates a strip.
    unsafe extern "system" fn strip_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => unsafe { DefWindowProcW(h, msg, wp, lp) },
        }
    }

    fn instance() -> Option<HINSTANCE> {
        unsafe { GetModuleHandleW(None) }.ok().map(|m| HINSTANCE(m.0))
    }

    /// The strips' window class. Only a registration that worked is remembered: a failed one is
    /// tried again at the next recording, and a class already registered counts as done.
    fn class() -> bool {
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError};
        static REGISTERED: AtomicBool = AtomicBool::new(false);
        if REGISTERED.load(Ordering::Relaxed) {
            return true;
        }
        let class = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(strip_proc), hInstance: instance().unwrap_or_default(), lpszClassName: CLASS, ..Default::default() };
        let ok = unsafe { RegisterClassExW(&class) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS };
        if ok {
            REGISTERED.store(true, Ordering::Relaxed);
        }
        ok
    }

    /// One strip, in this order, which is what keeps it from ever being shown blank: created
    /// hidden and layered (never made layered later), kept out of capture, given its pixels with
    /// `UpdateLayeredWindow` (see [`set_pixels`]), and only then shown topmost without activation.
    pub fn open_strip(s: &StripPixels) -> OpenedStrip {
        let failed = |note: String| OpenedStrip { window: None, note, pixels_set: false, excluded: false, visible: false };
        if !class() {
            return failed("RegisterClassExW failed".into());
        }
        let r = s.rect;
        // Layered from creation, topmost, never activated, a tool window (no taskbar button,
        // not in Alt+Tab), click-through. Created hidden.
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let h = match unsafe { CreateWindowExW(ex, CLASS, w!("Mythicshot glass band"), WS_POPUP, r.x, r.y, r.w as i32, r.h as i32, None, None, instance(), None) } {
            Ok(h) => h,
            Err(e) => return failed(format!("CreateWindowExW failed: {e}")),
        };
        let id = h.0 as WindowId;
        // Out of every capture before it is ever shown.
        let mut excluded = super::exclude_from_capture(id);
        let pixels_set = set_pixels(h, s);
        // Shown only now, with its pixels in place: topmost, not activated, where it already is.
        unsafe {
            let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW);
        }
        if !excluded {
            excluded = super::exclude_from_capture(id);
        }
        let visible = unsafe { IsWindowVisible(h) }.as_bool();
        OpenedStrip { window: Some(id), note: super::composition_note(id), pixels_set, excluded, visible }
    }

    /// Hand a strip's pixels to Windows: a 32-bit top-down bitmap with premultiplied alpha, put
    /// on the strip's rectangle with per-pixel alpha.
    fn set_pixels(h: HWND, s: &StripPixels) -> bool {
        let r = s.rect;
        if s.pixels.len() != r.w as usize * r.h as usize * 4 {
            return false;
        }
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return false;
            }
            let mem = CreateCompatibleDC(Some(screen));
            if mem.is_invalid() {
                ReleaseDC(None, screen);
                return false;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: r.w as i32, biHeight: -(r.h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let ok = match CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(dib) => {
                    let ok = !bits.is_null() && {
                        std::ptr::copy_nonoverlapping(s.pixels.as_ptr(), bits as *mut u8, s.pixels.len());
                        let old = SelectObject(mem, HGDIOBJ(dib.0));
                        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
                        let at = POINT { x: r.x, y: r.y };
                        let size = SIZE { cx: r.w as i32, cy: r.h as i32 };
                        let from = POINT { x: 0, y: 0 };
                        let ok = UpdateLayeredWindow(h, Some(screen), Some(&at), Some(&size), Some(mem), Some(&from), COLORREF(0), Some(&blend), ULW_ALPHA).is_ok();
                        SelectObject(mem, old);
                        ok
                    };
                    // Deleted whether or not its bits came back.
                    let _ = DeleteObject(HGDIOBJ(dib.0));
                    ok
                }
                Err(_) => false,
            };
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            ok
        }
    }

    pub fn raise(id: WindowId) {
        unsafe {
            let _ = SetWindowPos(HWND(id as *mut _), Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW);
        }
    }

    pub fn close(id: WindowId, own_thread: bool) {
        let h = HWND(id as *mut _);
        unsafe {
            if own_thread {
                let _ = DestroyWindow(h);
            } else {
                let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
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
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("set foreground {hwnd}")) {
        return;
    }
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
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("set autostart {on}")) {
        return Ok(());
    }
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
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("open path {}", path.display())) {
        return;
    }
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
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("reveal {}", path.display())) {
        return;
    }
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
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("open folder {}", path.display())) {
        return;
    }
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

    /// Only a window in its normal state gives a size: not maximized, not minimized, and with its
    /// window rectangle the size of its restore rectangle (a snapped window's is not).
    #[test]
    fn only_a_window_in_its_normal_state_gives_its_size() {
        let (normal, maximized, minimized) = (1, 3, 2);
        let rect = (936, 779);
        assert_eq!(normal_state_client(normal, false, rect, rect, (920, 740)), Some((920, 740)));
        assert_eq!(normal_state_client(maximized, false, (2576, 1408), rect, (2560, 1369)), None, "maximized");
        assert_eq!(normal_state_client(minimized, true, (160, 28), rect, (0, 0)), None, "minimized");
        assert_eq!(normal_state_client(normal, true, rect, rect, (920, 740)), None, "iconic whatever the placement says");
        assert_eq!(normal_state_client(normal, false, (1296, 1408), rect, (1280, 1369)), None, "snapped: the window is not its restore rectangle");
        assert_eq!(normal_state_client(normal, false, rect, rect, (0, 740)), None, "no client area");
    }

    /// On a real window that is never shown (created hidden, in this thread, aware of each
    /// monitor's DPI as the app is), at a few sizes: in its normal state it gives its client size
    /// and its DPI; it is a window of this process with its title; and, hidden, the lookup for the
    /// main window passes it over, as it passes over the app's hidden root window of the same
    /// title. Destroyed, nothing is read from its handle. (A visible window, and a snapped or
    /// maximized one, cannot be made here without showing it.)
    #[cfg(windows)]
    #[test]
    fn a_hidden_normal_window_gives_its_client_size_and_is_passed_over_by_the_lookup() {
        use windows::Win32::Foundation::{HWND, RECT};
        use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, GetClientRect, IsWindowVisible, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW};
        use windows::core::w;
        const TITLE: &str = "Mythicshot normal size test";
        let before = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        // A plain window of a system class, far off every monitor, without WS_VISIBLE.
        let h: HWND = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("Mythicshot normal size test"), WS_OVERLAPPEDWINDOW, -32000, -32000, 900, 700, None, None, None, None) }.expect("a hidden window");
        let id = h.0 as WindowId;
        assert!(is_own_window_titled(id, TITLE));
        assert!(!is_own_window_titled(id, "Mythicshot"), "the exact title only");
        assert_eq!(find_window(TITLE), Some(id), "found by its title when hidden ones count");
        for (w, hgt) in [(900, 700), (1380, 1110), (643, 421), (2001, 1501)] {
            unsafe { SetWindowPos(h, None, 0, 0, w, hgt, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_HIDEWINDOW) }.unwrap();
            assert!(!unsafe { IsWindowVisible(h) }.as_bool(), "never shown");
            let mut client = RECT::default();
            unsafe { GetClientRect(h, &mut client) }.unwrap();
            let client = (client.right - client.left, client.bottom - client.top);
            let (nw, nh, dpi) = normal_client_px(id).expect("readable");
            assert!(dpi >= 96, "{dpi}");
            assert_eq!((nw, nh), client, "{w}x{hgt} at {dpi} DPI");
            assert_eq!(find_visible_own_window(TITLE, client, 2), None, "{w}x{hgt}: hidden, so not the main window");
        }
        unsafe { DestroyWindow(h) }.unwrap();
        assert_eq!(normal_client_px(id), None);
        assert!(!is_own_window_titled(id, TITLE), "gone");
        if !before.is_invalid() {
            unsafe { SetThreadDpiAwarenessContext(before) };
        }
    }
}
