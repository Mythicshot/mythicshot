//! Opening a picture file in the editor: the tray's "Open image from file…" and the Home page's
//! Open image tile, which both ask for a file with the same dialog.

use std::path::{Path, PathBuf};

use crate::dialogs;

/// The picture files the dialog offers, by extension.
pub const SUPPORTED: [&str; 6] = ["png", "jpg", "jpeg", "bmp", "gif", "webp"];

/// What the main window asks the app to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenImage {
    /// Ask for a file, as the tray's "Open image from file…" does: the Open image tile.
    Pick,
}

/// Ask for a picture file in `dir`, the captures folder: the dialog the tray's "Open image from
/// file…" opens.
pub fn pick(dir: &Path) -> Option<PathBuf> {
    dialogs::pick_file(rfd::FileDialog::new().add_filter("Images", &SUPPORTED).set_directory(dir), "open image")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mythicshot_destinations::stubs;

    #[test]
    fn picking_asks_the_open_image_dialog_and_no_real_dialog_opens() {
        let tmp = tempfile::tempdir().unwrap();
        stubs::take();
        assert_eq!(pick(tmp.path()), None, "a test build answers as if cancelled");
        assert_eq!(stubs::take(), ["open dialog: open image"]);
    }
}
