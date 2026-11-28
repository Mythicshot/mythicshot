//! The tray icon and its menu (screen 1 of the mockup).

use std::collections::HashMap;

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use mythicshot_destinations::DestId;

use crate::hotkeys::{self, Action};
use crate::settings::{Settings, TrayLeftClick};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Region,
    LastRegion,
    Window,
    FullScreen,
    Record,
    RecordLastRegion,
    RecordWindow,
    RecordFullScreen,
    PauseRecording,
    /// Only does something while recording.
    LiveDraw,
    /// Index into `xcap::Monitor::all()` order at the time the menu was built.
    Display(usize),
    FromClipboard,
    OpenFile,
    ToggleDestination(DestId),
    ToggleMagnifier,
    /// Open the main window on its Home page.
    OpenApp,
    /// Open the main window on its Preferences page.
    Preferences,
    OpenFolder,
    /// The first entry while a newer version is available: what "Update now" does.
    Update,
    /// Let go of the global shortcuts and register them again, on a click in the menu only.
    ReconnectShortcuts,
    Exit,
}

/// The menu's last entries, in order: "Reconnect shortcuts" just above Exit.
pub const FOOT: [(&str, Command); 2] = [("Reconnect shortcuts", Command::ReconnectShortcuts), ("Exit", Command::Exit)];

impl Command {
    /// Whether running the command puts something new on the screen or takes a picture of it: a
    /// capture, a recording, an editor, a file dialog, a folder, the main window. A capture that
    /// is waiting for the main window to be gone is dropped when one of these runs, so the two
    /// never both fire and the waiting one never photographs what the other opened.
    pub fn changes_the_screen(self) -> bool {
        !matches!(self, Command::PauseRecording | Command::LiveDraw | Command::ToggleDestination(_) | Command::ToggleMagnifier | Command::ReconnectShortcuts)
    }

    pub fn from_action(a: Action) -> Command {
        match a {
            Action::CaptureRegion => Command::Region,
            Action::CaptureLastRegion => Command::LastRegion,
            Action::CaptureWindow => Command::Window,
            Action::CaptureFullScreen => Command::FullScreen,
            Action::RecordRegion => Command::Record,
            Action::RecordLastRegion => Command::RecordLastRegion,
            Action::RecordWindow => Command::RecordWindow,
            Action::RecordFullScreen => Command::RecordFullScreen,
            Action::PauseRecording => Command::PauseRecording,
            Action::LiveDraw => Command::LiveDraw,
        }
    }
}

/// What is on screen that a tray click must not disturb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Activity {
    /// The region selector covers the desktop.
    pub overlay: bool,
    /// A recording session: its countdown, or the recording itself.
    pub session: bool,
    /// An "after capture" picker is waiting for a choice.
    pub picker: bool,
    /// A capture started from the main window is waiting for the window to be off the screen.
    pub pending: bool,
}

impl Activity {
    /// Something is being captured or chosen: the main window must not open over it.
    pub fn busy(self) -> bool {
        self.overlay || self.session || self.picker || self.pending
    }
}

/// Whether a command may run now. The two that open the main window, from the tray menu as much as
/// from a click, do nothing while something is being captured or chosen: the window would land in
/// a recording, take focus from the app being recorded, or be in the picture a waiting capture is
/// about to take. Everything else is left to the command itself.
pub fn allowed(cmd: Command, activity: Activity) -> bool {
    !(matches!(cmd, Command::OpenApp | Command::Preferences) && activity.busy())
}

/// Whether a capture that waited for the main window to be gone may start now: not when a region
/// is already being selected or a recording session exists.
pub fn pending_may_run(activity: Activity) -> bool {
    !activity.overlay && !activity.session
}

/// What a left-click on the tray icon runs, or `None` when the click is ignored.
///
/// Opening the app is ignored while a region is being selected, a recording is counting down or
/// running, or a picker is waiting: the window would cover the capture, take focus from the app
/// being recorded, or land in the recording. A region capture is ignored while the selector is
/// already up, a recording session exists or another capture is about to start, since a second
/// one cannot start then.
pub fn left_click(choice: TrayLeftClick, activity: Activity) -> Option<Command> {
    match choice {
        TrayLeftClick::OpenApp if !allowed(Command::OpenApp, activity) => None,
        TrayLeftClick::OpenApp => Some(Command::OpenApp),
        TrayLeftClick::CaptureRegion if activity.overlay || activity.session || activity.pending => None,
        TrayLeftClick::CaptureRegion => Some(Command::Region),
    }
}

pub struct Tray {
    _icon: TrayIcon,
    /// The menu, to put the update entry at its top and take it away again.
    menu: Menu,
    /// The update entry and the separator under it, while they are in the menu, and its label.
    update: Option<(MenuItem, PredefinedMenuItem, String)>,
    commands: HashMap<MenuId, Command>,
    labelled: Vec<(Action, MenuItem)>,
    destinations: Vec<(DestId, CheckMenuItem)>,
    magnifier: CheckMenuItem,
}

fn label(text: &str, binding: &str) -> String {
    let keys = hotkeys::display(binding);
    if keys.is_empty() { text.to_string() } else { format!("{text}\t{keys}") }
}

impl Tray {
    pub fn new(settings: &Settings, displays: &[String]) -> Result<Self, String> {
        let mut commands = HashMap::new();
        let mut labelled = Vec::new();
        let mut item = |text: &str, cmd: Command, enabled: bool| {
            let it = MenuItem::new(text, enabled, None);
            commands.insert(it.id().clone(), cmd);
            it
        };

        let open_app = item("Open Mythicshot", Command::OpenApp, true);
        let region = item(&label("Capture region", settings.binding(Action::CaptureRegion)), Command::Region, true);
        let last = item(&label("Capture last region", settings.binding(Action::CaptureLastRegion)), Command::LastRegion, true);
        let window = item(&label("Capture window", settings.binding(Action::CaptureWindow)), Command::Window, true);
        let all_screens = item(&label("All screens", settings.binding(Action::CaptureFullScreen)), Command::FullScreen, true);
        let display_items: Vec<MenuItem> = displays.iter().enumerate().map(|(i, name)| item(name, Command::Display(i), true)).collect();
        let from_clipboard = item("Capture from clipboard", Command::FromClipboard, true);
        let open_file = item("Open image from file…", Command::OpenFile, true);
        let record = Submenu::new("Record GIF", true);
        let rec_region = item(&label("Region", settings.binding(Action::RecordRegion)), Command::Record, true);
        let rec_last = item(&label("Last region", settings.binding(Action::RecordLastRegion)), Command::RecordLastRegion, true);
        let rec_window = item(&label("Window", settings.binding(Action::RecordWindow)), Command::RecordWindow, true);
        let rec_full = item(&label("Full screen", settings.binding(Action::RecordFullScreen)), Command::RecordFullScreen, true);
        for it in [&rec_region, &rec_last, &rec_window, &rec_full] {
            record.append(it).map_err(|e| e.to_string())?;
        }
        let prefs = item("Preferences…", Command::Preferences, true);
        let folder = item("Open captures folder", Command::OpenFolder, true);
        let foot: Vec<MenuItem> = FOOT.iter().map(|(text, cmd)| item(text, *cmd, true)).collect();

        let magnifier = CheckMenuItem::new("Show magnifier", true, settings.magnifier, None);
        commands.insert(magnifier.id().clone(), Command::ToggleMagnifier);
        let destinations: Vec<(DestId, CheckMenuItem)> = DestId::ALL
            .iter()
            .map(|d| {
                let it = CheckMenuItem::new(d.label(), true, settings.has_destination(*d), None);
                commands.insert(it.id().clone(), Command::ToggleDestination(*d));
                (*d, it)
            })
            .collect();

        let full = Submenu::new("Capture full screen", true);
        full.append(&all_screens).map_err(|e| e.to_string())?;
        for d in &display_items {
            full.append(d).map_err(|e| e.to_string())?;
        }
        let quick = Submenu::new("After capture", true);
        for (_, it) in &destinations {
            quick.append(it).map_err(|e| e.to_string())?;
        }
        quick.append(&PredefinedMenuItem::separator()).map_err(|e| e.to_string())?;
        quick.append(&magnifier).map_err(|e| e.to_string())?;

        let menu = Menu::new();
        let sep = PredefinedMenuItem::separator;
        menu.append(&open_app).map_err(|e| e.to_string())?;
        menu.append(&sep()).map_err(|e| e.to_string())?;
        for it in [&region as &dyn tray_icon::menu::IsMenuItem, &last, &window, &full, &from_clipboard, &open_file] {
            menu.append(it).map_err(|e| e.to_string())?;
        }
        menu.append(&sep()).map_err(|e| e.to_string())?;
        menu.append(&record).map_err(|e| e.to_string())?;
        menu.append(&sep()).map_err(|e| e.to_string())?;
        for it in [&quick as &dyn tray_icon::menu::IsMenuItem, &prefs, &folder] {
            menu.append(it).map_err(|e| e.to_string())?;
        }
        menu.append(&sep()).map_err(|e| e.to_string())?;
        for it in &foot {
            menu.append(it).map_err(|e| e.to_string())?;
        }

        labelled.push((Action::CaptureRegion, region));
        labelled.push((Action::CaptureLastRegion, last));
        labelled.push((Action::CaptureWindow, window));
        labelled.push((Action::CaptureFullScreen, all_screens));
        labelled.push((Action::RecordRegion, rec_region));
        labelled.push((Action::RecordLastRegion, rec_last));
        labelled.push((Action::RecordWindow, rec_window));
        labelled.push((Action::RecordFullScreen, rec_full));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(false)
            .with_tooltip("Mythicshot")
            .with_icon(app_icon())
            .build()
            .map_err(|e| e.to_string())?;

        Ok(Self { _icon: icon, menu, update: None, commands, labelled, destinations, magnifier })
    }

    /// Show `label` as the menu's first entry ([`Command::Update`]), or no such entry for None.
    /// Nothing changes when the menu already shows it.
    pub fn set_update(&mut self, label: Option<String>) {
        if self.update.as_ref().map(|(_, _, l)| l) == label.as_ref() {
            return;
        }
        if let Some((item, sep, _)) = self.update.take() {
            self.commands.remove(item.id());
            let _ = self.menu.remove(&item);
            let _ = self.menu.remove(&sep);
        }
        if let Some(label) = label {
            let item = MenuItem::new(&label, true, None);
            let sep = PredefinedMenuItem::separator();
            if self.menu.insert(&item, 0).is_ok() {
                let _ = self.menu.insert(&sep, 1);
                self.commands.insert(item.id().clone(), Command::Update);
                self.update = Some((item, sep, label));
            }
        }
    }

    pub fn command(&self, id: &MenuId) -> Option<Command> {
        self.commands.get(id).copied()
    }

    /// Refresh shortcut text and check marks after settings change.
    pub fn sync(&self, settings: &Settings) {
        for (action, item) in &self.labelled {
            let text = match action {
                Action::CaptureFullScreen => "All screens",
                Action::RecordRegion => "Region",
                Action::RecordLastRegion => "Last region",
                Action::RecordWindow => "Window",
                Action::RecordFullScreen => "Full screen",
                a => a.label(),
            };
            item.set_text(label(text, settings.binding(*action)));
        }
        for (d, it) in &self.destinations {
            it.set_checked(settings.has_destination(*d));
        }
        self.magnifier.set_checked(settings.magnifier);
    }
}

/// The tray icon, at the size the screen's scaling shows it.
pub fn app_icon() -> Icon {
    crate::app_icon::tray()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Activity = Activity { overlay: false, session: false, picker: false, pending: false };

    #[test]
    fn the_menu_items_that_open_the_window_do_nothing_while_busy() {
        let busy = [Activity { overlay: true, ..IDLE }, Activity { session: true, ..IDLE }, Activity { picker: true, ..IDLE }, Activity { pending: true, ..IDLE }];
        for a in busy {
            assert!(a.busy(), "{a:?}");
            for cmd in [Command::OpenApp, Command::Preferences] {
                assert!(!allowed(cmd, a), "{cmd:?} during {a:?}");
            }
            // Nothing else is held back here.
            for cmd in [Command::Region, Command::Record, Command::PauseRecording, Command::LiveDraw, Command::OpenFolder, Command::ToggleMagnifier, Command::Exit] {
                assert!(allowed(cmd, a), "{cmd:?} during {a:?}");
            }
        }
        assert!(allowed(Command::OpenApp, IDLE) && allowed(Command::Preferences, IDLE));
    }

    #[test]
    fn the_wait_before_a_capture_counts_as_busy() {
        let waiting = Activity { pending: true, ..IDLE };
        assert!(waiting.busy());
        assert_eq!(left_click(TrayLeftClick::OpenApp, waiting), None);
        assert_eq!(left_click(TrayLeftClick::CaptureRegion, waiting), None, "one capture is already about to start");
    }

    #[test]
    fn a_waiting_capture_starts_only_when_nothing_else_did() {
        assert!(pending_may_run(IDLE));
        // Its own wait, and a picker from an earlier capture, do not hold it back.
        assert!(pending_may_run(Activity { pending: true, ..IDLE }));
        assert!(pending_may_run(Activity { picker: true, ..IDLE }));
        assert!(!pending_may_run(Activity { overlay: true, ..IDLE }));
        assert!(!pending_may_run(Activity { session: true, ..IDLE }));
    }

    #[test]
    fn only_switches_and_recording_controls_leave_a_waiting_capture_alone() {
        for cmd in [Command::Region, Command::LastRegion, Command::Window, Command::FullScreen, Command::Record, Command::RecordLastRegion, Command::RecordWindow, Command::RecordFullScreen, Command::Display(0), Command::FromClipboard, Command::OpenFile, Command::OpenApp, Command::Preferences, Command::OpenFolder, Command::Update, Command::Exit] {
            assert!(cmd.changes_the_screen(), "{cmd:?}");
        }
        for cmd in [Command::PauseRecording, Command::LiveDraw, Command::ToggleMagnifier, Command::ToggleDestination(DestId::File), Command::ReconnectShortcuts] {
            assert!(!cmd.changes_the_screen(), "{cmd:?}");
        }
    }

    /// "Reconnect shortcuts" is the entry just above Exit, and the menu is its only way in: no
    /// left click, shortcut or busy state runs it, and nothing holds it back.
    #[test]
    fn reconnect_shortcuts_sits_just_above_exit_and_only_a_click_on_it_runs_it() {
        assert_eq!(FOOT, [("Reconnect shortcuts", Command::ReconnectShortcuts), ("Exit", Command::Exit)]);
        for a in Action::ALL {
            assert_ne!(Command::from_action(*a), Command::ReconnectShortcuts, "{a:?}");
        }
        for choice in TrayLeftClick::ALL {
            assert_ne!(left_click(choice, IDLE), Some(Command::ReconnectShortcuts));
        }
        assert!(allowed(Command::ReconnectShortcuts, IDLE) && allowed(Command::ReconnectShortcuts, Activity { overlay: true, session: true, picker: true, pending: true }));
    }

    #[test]
    fn a_left_click_follows_the_setting_when_nothing_is_going_on() {
        assert_eq!(left_click(TrayLeftClick::OpenApp, IDLE), Some(Command::OpenApp));
        assert_eq!(left_click(TrayLeftClick::CaptureRegion, IDLE), Some(Command::Region));
        assert_eq!(left_click(TrayLeftClick::default(), IDLE), Some(Command::OpenApp));
    }

    #[test]
    fn a_left_click_never_opens_the_window_over_a_capture() {
        let overlay = Activity { overlay: true, ..IDLE };
        // The countdown and the recording are one session.
        let session = Activity { session: true, ..IDLE };
        let picker = Activity { picker: true, ..IDLE };
        for busy in [overlay, session, picker, Activity { overlay: true, session: true, picker: true, pending: true }] {
            assert!(busy.busy());
            assert_eq!(left_click(TrayLeftClick::OpenApp, busy), None, "{busy:?}");
        }
        assert!(!IDLE.busy());
    }

    #[test]
    fn a_region_click_does_not_start_a_second_capture() {
        assert_eq!(left_click(TrayLeftClick::CaptureRegion, Activity { overlay: true, ..IDLE }), None);
        assert_eq!(left_click(TrayLeftClick::CaptureRegion, Activity { session: true, ..IDLE }), None);
        // A waiting picker never stopped a new capture, and still does not.
        assert_eq!(left_click(TrayLeftClick::CaptureRegion, Activity { picker: true, ..IDLE }), Some(Command::Region));
    }
}
