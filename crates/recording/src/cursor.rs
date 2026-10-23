//! The mouse pointer: where it is, and a small arrow drawn into frames.

use image::RgbaImage;

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

/// A standard arrow pointer: `#` outline, `o` fill, `.` transparent.
const ARROW: [&str; 19] = [
    "#..........",
    "##.........",
    "#o#........",
    "#oo#.......",
    "#ooo#......",
    "#oooo#.....",
    "#ooooo#....",
    "#oooooo#...",
    "#ooooooo#..",
    "#oooooooo#.",
    "#ooooo#####",
    "#oo#oo#....",
    "#o#.#oo#...",
    "##..#oo#...",
    "#....#oo#..",
    ".....#oo#..",
    "......#oo#.",
    "......#oo#.",
    ".......##..",
];

/// Draw the pointer with its tip at `(x, y)`, clipped to the frame.
pub fn draw(img: &mut RgbaImage, x: i32, y: i32) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    for (dy, row) in ARROW.iter().enumerate() {
        for (dx, ch) in row.chars().enumerate() {
            let color = match ch {
                '#' => [0, 0, 0, 255],
                'o' => [255, 255, 255, 255],
                _ => continue,
            };
            let (px, py) = (x + dx as i32, y + dy as i32);
            if px >= 0 && py >= 0 && px < w && py < h {
                img.put_pixel(px as u32, py as u32, image::Rgba(color));
            }
        }
    }
}
