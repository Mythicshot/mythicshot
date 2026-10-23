//! Where the mouse pointer is. Drawing it into frames is the overlay renderer's job.

#[cfg(windows)]
pub fn position() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| (p.x, p.y)) }
}

#[cfg(not(windows))]
pub fn position() -> Option<(i32, i32)> {
    None
}
