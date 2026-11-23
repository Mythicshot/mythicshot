//! The system's file dialogs. Every one the app opens goes through here, so a test build can
//! never open one on the desktop: there each is recorded (`what`) and answered as if cancelled.

use std::path::PathBuf;

/// Ask for a file to open.
pub fn pick_file(dialog: rfd::FileDialog, what: &str) -> Option<PathBuf> {
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("open dialog: {what}")) {
        let _ = dialog;
        return None;
    }
    let _ = what;
    dialog.pick_file()
}

/// Ask for a folder.
pub fn pick_folder(dialog: rfd::FileDialog, what: &str) -> Option<PathBuf> {
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("folder dialog: {what}")) {
        let _ = dialog;
        return None;
    }
    let _ = what;
    dialog.pick_folder()
}

/// Ask where to save a file.
pub fn save_file(dialog: rfd::FileDialog, what: &str) -> Option<PathBuf> {
    #[cfg(test)]
    if mythicshot_destinations::stubs::stubbed(format!("save dialog: {what}")) {
        let _ = dialog;
        return None;
    }
    let _ = what;
    dialog.save_file()
}

/// Each stand-in a test build uses for the desktop, called through the app's real entry point:
/// it records what was asked for and answers harmlessly, and nothing reaches the desktop.
#[cfg(test)]
mod tests {
    use mythicshot_destinations::{self as dest, DestId, Sent, stubs};

    fn picture() -> image::RgbaImage {
        image::RgbaImage::new(4, 3)
    }

    fn context(dir: &std::path::Path) -> dest::Context {
        let mut s = crate::settings::Settings::default();
        s.output_dir = dir.to_path_buf();
        s.dest_context("Stub")
    }

    #[test]
    fn the_app_refuses_to_run_as_a_test_build() {
        // The flag main() checks before anything else is on in every test build.
        assert!(dest::STUBBED);
    }

    #[test]
    fn save_as_asks_no_file_dialog() {
        let tmp = tempfile::tempdir().unwrap();
        stubs::take();
        assert_eq!(dest::send(DestId::FileAs, &picture(), &context(tmp.path())), Ok(Sent::Cancelled));
        let asked = stubs::take();
        assert!(matches!(asked.as_slice(), [a] if a.starts_with("save as dialog: ")), "{asked:?}");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "nothing saved");
    }

    #[test]
    fn the_clipboard_is_never_written_or_read() {
        stubs::take();
        assert_eq!(dest::copy_to_clipboard(&picture()), Ok(()));
        assert_eq!(dest::image_from_clipboard().err().as_deref(), Some("There is no image on the clipboard"));
        assert!(!crate::share_ui::copy_to_clipboard("a link"));
        assert_eq!(stubs::take(), ["clipboard image 4x3", "read clipboard image", "clipboard text a link"]);
        // The Clipboard destination goes the same way.
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(dest::send(DestId::Clipboard, &picture(), &context(tmp.path())), Ok(Sent::Copied));
        assert_eq!(stubs::take(), ["clipboard image 4x3"]);
    }

    #[test]
    fn nothing_is_printed() {
        let tmp = tempfile::tempdir().unwrap();
        stubs::take();
        assert_eq!(dest::send(DestId::Print, &picture(), &context(tmp.path())), Ok(Sent::Printing));
        let asked = stubs::take();
        assert!(matches!(asked.as_slice(), [a] if a.starts_with("print ")), "{asked:?}");
    }

    #[test]
    fn no_mail_app_is_opened() {
        let tmp = tempfile::tempdir().unwrap();
        stubs::take();
        assert_eq!(dest::send(DestId::Email, &picture(), &context(tmp.path())), Ok(Sent::EmailOpened));
        let asked = stubs::take();
        assert!(matches!(asked.as_slice(), [a] if a.starts_with("email ") && a.ends_with("(Stub)")), "{asked:?}");
    }

    #[test]
    fn no_browser_is_opened() {
        stubs::take();
        assert_eq!(dest::open_url(crate::menu_bar::DOWNLOAD_PAGE), Ok(()));
        assert_eq!(stubs::take(), [format!("open url {}", crate::menu_bar::DOWNLOAD_PAGE)]);
        // A link that would be refused is refused before anything is asked.
        assert!(dest::open_url("calc.exe").is_err());
        assert!(stubs::take().is_empty());
    }

    #[test]
    fn no_file_manager_or_app_is_started_and_no_folder_is_made() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("not made");
        stubs::take();
        crate::platform::open_folder(&missing);
        crate::platform::open_path(&missing.join("a.html"));
        crate::platform::reveal(&missing.join("a.html"));
        assert!(!missing.exists(), "open_folder made no folder");
        let asked = stubs::take();
        assert_eq!(asked.len(), 3, "{asked:?}");
        assert!(asked[0].starts_with("open folder ") && asked[1].starts_with("open path ") && asked[2].starts_with("reveal "), "{asked:?}");
    }

    #[cfg(windows)]
    #[test]
    fn no_setting_of_windows_and_no_pointer_or_focus_is_changed() {
        stubs::take();
        let before = crate::platform::autostart_enabled();
        assert_eq!(crate::platform::set_autostart(!before), Ok(()));
        assert_eq!(crate::platform::autostart_enabled(), before, "the Run key was not touched");
        assert_eq!(crate::platform::set_snipping_tool_prtsc(true), Ok(()));
        let pointer = crate::platform::cursor_pos();
        crate::platform::nudge_cursor(5, 5);
        assert_eq!(crate::platform::cursor_pos(), pointer, "the pointer did not move");
        crate::platform::set_foreground(1);
        assert!(!crate::platform::take_foreground(1));
        assert_eq!(stubs::take(), [format!("set autostart {}", !before), "set snipping tool print screen true".to_string(), "nudge cursor 5 5".into(), "set foreground 1".into(), "take foreground 1".into()]);
    }

    #[test]
    fn no_file_dialog_of_the_app_opens() {
        stubs::take();
        assert_eq!(super::pick_file(rfd::FileDialog::new(), "a"), None);
        assert_eq!(super::pick_folder(rfd::FileDialog::new(), "b"), None);
        assert_eq!(super::save_file(rfd::FileDialog::new(), "c"), None);
        assert_eq!(stubs::take(), ["open dialog: a", "folder dialog: b", "save dialog: c"]);
    }

    #[test]
    fn the_credential_store_is_never_read_or_written() {
        stubs::take();
        assert_eq!(crate::publish::load_token(), Ok(None));
        assert_eq!(crate::publish::store_token("dp_0123456789abcdef0123456789abcdef01234567"), Ok(()));
        assert_eq!(crate::publish::forget_token(), Ok(()));
        assert_eq!(stubs::take(), ["credential store: read", "credential store: write", "credential store: delete"]);
    }

    #[test]
    fn the_users_settings_logs_projects_and_pictures_are_never_touched() {
        let ours = crate::settings::test_data_dir();
        assert!(ours.starts_with(std::env::temp_dir()), "{ours:?}");
        for (what, path) in [
            ("settings", crate::settings::Settings::path()),
            ("capture.log", crate::capture_log::path()),
            ("captures", crate::settings::Settings::default().output_dir),
            ("projects", crate::project::Store::default_root().unwrap()),
        ] {
            assert!(path.starts_with(&ours), "{what} at {path:?}");
        }
    }
}
