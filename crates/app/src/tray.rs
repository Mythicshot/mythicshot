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
    Exit,
}

impl Command {
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
}

impl Activity {
    /// Something is being captured or chosen: the main window must not open over it.
    pub fn busy(self) -> bool {
        self.overlay || self.session || self.picker
    }
}

/// What a left-click on the tray icon runs, or `None` when the click is ignored.
///
/// Opening the app is ignored while a region is being selected, a recording is counting down or
/// running, or a picker is waiting: the window would cover the capture, take focus from the app
/// being recorded, or land in the recording. A region capture is ignored while the selector is
/// already up or a recording session exists, since a second one cannot start then.
pub fn left_click(choice: TrayLeftClick, activity: Activity) -> Option<Command> {
    match choice {
        TrayLeftClick::OpenApp if activity.busy() => None,
        TrayLeftClick::OpenApp => Some(Command::OpenApp),
        TrayLeftClick::CaptureRegion if activity.overlay || activity.session => None,
        TrayLeftClick::CaptureRegion => Some(Command::Region),
    }
}

pub struct Tray {
    _icon: TrayIcon,
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
        let exit = item("Exit", Command::Exit, true);

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
        menu.append(&exit).map_err(|e| e.to_string())?;

        labelled.push((Action::CaptureRegion, region));
        labelled.push((Action::CaptureLastRegion, last));
        labelled.push((Action::CaptureWindow, window));
        labelled.push((Action::CaptureFullScreen, all_screens));
        labelled.push((Action::RecordRegion, rec_region));
        labelled.push((Action::RecordLastRegion, rec_last));
        labelled.push((Action::RecordWindow, rec_window));
        labelled.push((Action::RecordFullScreen, rec_full));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Mythicshot")
            .with_icon(app_icon())
            .build()
            .map_err(|e| e.to_string())?;

        Ok(Self { _icon: icon, commands, labelled, destinations, magnifier })
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

/// A 32×32 icon drawn in code: a green rounded square with a white lens.
pub fn app_icon() -> Icon {
    let (rgba, size) = icon_rgba(32);
    Icon::from_rgba(rgba, size, size).expect("valid icon")
}

pub fn icon_rgba(size: u32) -> (Vec<u8>, u32) {
    let mut px = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;
    let radius = s * 0.22;
    let c = s / 2.0;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (radius - fx).max(fx - (s - radius)).max(0.0);
            let dy = (radius - fy).max(fy - (s - radius)).max(0.0);
            let inside = (dx * dx + dy * dy).sqrt() <= radius;
            let d = ((fx - c).powi(2) + (fy - c).powi(2)).sqrt();
            let i = ((y * size + x) * 4) as usize;
            let color: [u8; 4] = if !inside {
                [0, 0, 0, 0]
            } else if d < s * 0.16 {
                [46, 125, 91, 255]
            } else if d < s * 0.30 {
                [255, 255, 255, 255]
            } else {
                [46, 125, 91, 255]
            };
            px[i..i + 4].copy_from_slice(&color);
        }
    }
    (px, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Activity = Activity { overlay: false, session: false, picker: false };

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
        for busy in [overlay, session, picker, Activity { overlay: true, session: true, picker: true }] {
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
