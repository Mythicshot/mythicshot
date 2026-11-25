//! The app's own icon, for the tray and the windows. Every size is read from
//! `assets/mythicshot.ico`, the same file the build puts in the exe: under 48
//! pixels it holds the brand's small mark, at 48 and 64 the standard mark, and at
//! 128 and 256 the rich one (see `scripts/make-brand-art.mjs`). The size shown is
//! picked to match the screen's scaling, so Windows does not have to stretch it.

use std::sync::Arc;

use eframe::egui;

const ICO: &[u8] = include_bytes!("../../../assets/mythicshot.ico");

/// Each picture in the file, smallest first: its side in pixels and its PNG bytes.
fn pictures() -> Vec<(u32, &'static [u8])> {
    let word = |at: usize| u16::from_le_bytes([ICO[at], ICO[at + 1]]) as usize;
    let long = |at: usize| u32::from_le_bytes([ICO[at], ICO[at + 1], ICO[at + 2], ICO[at + 3]]) as usize;
    let mut all: Vec<(u32, &'static [u8])> = (0..word(4))
        .map(|i| {
            let entry = 6 + 16 * i;
            // A side of 256 is written as 0.
            let side = if ICO[entry] == 0 { 256 } else { u32::from(ICO[entry]) };
            let (len, start) = (long(entry + 8), long(entry + 12));
            (side, &ICO[start..start + len])
        })
        .collect();
    all.sort_by_key(|(side, _)| *side);
    all
}

/// The smallest picture at least `want` pixels wide, or the largest there is.
fn picture(want: u32) -> (u32, &'static [u8]) {
    let all = pictures();
    all.iter().find(|(side, _)| *side >= want).or(all.last()).copied().expect("the icon file holds pictures")
}

/// The icon as RGBA pixels with its side, from the [`picture`] for `want`.
pub fn rgba(want: u32) -> (Vec<u8>, u32) {
    let (side, png) = picture(want);
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).expect("the icon file holds PNG pictures").to_rgba8();
    debug_assert_eq!((image.width(), image.height()), (side, side));
    (image.into_raw(), side)
}

/// The side of a small icon (title bar, tray) and of a large one (taskbar, Alt-Tab) at the screen's scaling.
#[cfg(windows)]
fn system_sizes() -> (u32, u32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXICON, SM_CXSMICON};
    let (small, large) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CXICON)) };
    (u32::try_from(small).ok().filter(|s| *s > 0).unwrap_or(16), u32::try_from(large).ok().filter(|s| *s > 0).unwrap_or(32))
}

#[cfg(not(windows))]
fn system_sizes() -> (u32, u32) {
    (32, 64)
}

/// The tray icon.
pub fn tray() -> tray_icon::Icon {
    let (rgba, side) = rgba(system_sizes().0);
    tray_icon::Icon::from_rgba(rgba, side, side).expect("valid icon")
}

/// The icon a window is given: on Windows it is the title bar's small icon, elsewhere the only one.
pub fn window() -> Arc<egui::IconData> {
    let (small, large) = system_sizes();
    let (rgba, side) = rgba(if cfg!(windows) { small } else { large });
    Arc::new(egui::IconData { rgba, width: side, height: side })
}

/// Give every window of the app its large and small icon, through the window class they all share
/// (`window` is any one of them). A window's own icon, set from [`window`], is only the small one;
/// the taskbar and Alt-Tab ask for the large one and would otherwise stretch the small.
#[cfg(windows)]
pub fn set_for_all_windows(window: &impl raw_window_handle::HasWindowHandle) {
    use raw_window_handle::RawWindowHandle;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, GCLP_HICON, GCLP_HICONSM, LR_DEFAULTCOLOR, SetClassLongPtrW};
    let Some(RawWindowHandle::Win32(handle)) = window.window_handle().ok().map(|h| h.as_raw()) else { return };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let (small, large) = system_sizes();
    for (index, want) in [(GCLP_HICON, large), (GCLP_HICONSM, small)] {
        let (side, png) = picture(want);
        // Windows reads a PNG as an icon directly. The icon lives as long as the class, so it is never freed.
        if let Ok(icon) = unsafe { CreateIconFromResourceEx(png, true, 0x0003_0000, side as i32, side as i32, LR_DEFAULTCOLOR) } {
            unsafe { SetClassLongPtrW(hwnd, index, icon.0 as isize) };
        }
    }
}

#[cfg(not(windows))]
pub fn set_for_all_windows(_window: &impl raw_window_handle::HasWindowHandle) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sizes drawn from the small mark, the standard mark and the rich mark.
    const SMALL: [u32; 5] = [16, 20, 24, 32, 40];
    const STANDARD: [u32; 2] = [48, 64];
    const RICH: [u32; 2] = [128, 256];

    #[test]
    fn the_icon_file_holds_every_size_as_a_square_picture_of_that_size() {
        assert_eq!(&ICO[..4], [0, 0, 1, 0], "an .ico header");
        let all = pictures();
        assert_eq!(all.iter().map(|(side, _)| *side).collect::<Vec<_>>(), SMALL.into_iter().chain(STANDARD).chain(RICH).collect::<Vec<_>>());
        for (side, png) in all {
            let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).unwrap().to_rgba8();
            assert_eq!((image.width(), image.height()), (side, side));
            // Drawn in the middle, see-through in the corners.
            assert_eq!(image.get_pixel(0, 0)[3], 0, "{side}");
            assert_eq!(image.get_pixel(side / 2, side / 2)[3], 255, "{side}");
        }
    }

    #[test]
    fn each_size_is_drawn_from_the_mark_the_brand_names_for_it() {
        // The small mark's lens is plain; the standard and rich marks have a gold ring at its centre.
        // The standard mark's facets are one purple; the rich mark's each have their own.
        let (gold, dark, purple, rich_top) = ([0xf5, 0xb8, 0x41], [0x16, 0x12, 0x1c], [0x6d, 0x4a, 0xe8], [0x92, 0x7e, 0xe0]);
        for side in SMALL.into_iter().chain(STANDARD).chain(RICH) {
            let (rgba, _) = rgba(side);
            let px = |x: u32, y: u32| -> [u8; 3] {
                let at = ((y * side + x) * 4) as usize;
                [rgba[at], rgba[at + 1], rgba[at + 2]]
            };
            let centre = px(side / 2, (side as f32 * 0.508) as u32);
            let colours: std::collections::HashSet<[u8; 3]> = rgba.chunks_exact(4).filter(|p| p[3] == 255).map(|p| [p[0], p[1], p[2]]).collect();
            if SMALL.contains(&side) {
                assert_eq!(centre, dark, "{side}: the small mark's plain lens");
            } else {
                assert_eq!(centre, gold, "{side}: the ring in the lens");
            }
            assert_eq!(colours.contains(&purple), !RICH.contains(&side), "{side}: one purple for the facets");
            assert_eq!(colours.contains(&rich_top), RICH.contains(&side), "{side}: the rich mark's lit facet");
        }
    }

    #[test]
    fn a_wanted_size_gets_the_next_one_up_and_never_more_than_the_largest() {
        for (want, side) in [(1, 16), (16, 16), (17, 20), (20, 20), (24, 24), (28, 32), (32, 32), (40, 40), (41, 48), (48, 48), (64, 64), (96, 128), (256, 256), (400, 256)] {
            let (rgba, got) = rgba(want);
            assert_eq!(got, side, "wanted {want}");
            assert_eq!(rgba.len(), (side * side * 4) as usize);
        }
    }

    #[test]
    fn the_window_and_tray_icons_decode_to_the_size_the_screen_asks_for() {
        let (small, large) = system_sizes();
        let icon = window();
        assert_eq!(icon.width, icon.height);
        assert_eq!(icon.width, picture(if cfg!(windows) { small } else { large }).0);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        assert!(icon.rgba.chunks_exact(4).any(|p| p[3] == 255) && icon.rgba.chunks_exact(4).any(|p| p[3] == 0));
        // Building the tray icon checks its pixels against its size.
        let _ = tray();
    }

    /// The details the build embeds beside the icon, which the file's Properties show.
    #[test]
    fn the_file_properties_name_the_company_and_the_product_alone() {
        let build = include_str!("../build.rs");
        assert!(build.contains(r#".set("CompanyName", "Mythicshot, LLC")"#), "{build}");
        assert!(build.contains(r#".set("LegalCopyright", "Copyright (c) 2026 Mythicshot, LLC. Licensed under the GNU AGPL 3.0 or later.")"#), "{build}");
        assert_eq!(env!("CARGO_PKG_DESCRIPTION"), "Screenshots, screen recordings and click-through tours for Windows");
    }
}
