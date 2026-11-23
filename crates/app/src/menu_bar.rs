//! The menu bar at the top of the main window and of every editor window: File, Edit,
//! Preferences and Help.
//!
//! The menus are a familiar home for what the app already does; they add nothing of their own.
//! Each window says what applies to it now ([`State`]); an entry that does not apply is shown
//! disabled, never hidden. A click comes back as an [`Entry`], which the window runs the way its
//! own button or key does, and what only the app can do goes to it as an [`AppRequest`].
//!
//! The menus take no keys: a shortcut shown beside an entry is the window's own, which works as
//! before whether the menu is open or not. egui has no Alt+letter access keys; the menus are
//! reached with Tab and opened with Enter or Space, and the arrow keys move inside an open menu.

use eframe::egui;

use crate::hotkeys::{self, Action, EditorAction};
use crate::main_window::Page;
use crate::settings_ui::Tab;
use crate::theme;
use crate::tray::Command;

/// The page "Download page" opens in the browser.
pub const DOWNLOAD_PAGE: &str = "https://mythicshot.com/download/";

/// The hover text of "Properties" where there is nothing to set yet. Approved wording.
pub const NOTHING_TO_SET: &str = "Nothing to set here yet";

/// The four menus, left to right.
pub const TITLES: [&str; 4] = ["File", "Edit", "Preferences", "Help"];

/// One entry of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    CaptureRegion,
    RecordRegion,
    OpenTours,
    Save,
    Export,
    PublishTour,
    CloseWindow,
    Quit,
    Undo,
    Redo,
    Copy,
    Delete,
    Properties,
    /// A section of the Preferences page.
    Preferences(Tab),
    Workspace,
    Troubleshooting,
    LogFolder,
    DownloadPage,
    About,
}

impl Entry {
    pub fn label(self) -> &'static str {
        match self {
            Entry::CaptureRegion => "Capture region",
            Entry::RecordRegion => "Record region",
            Entry::OpenTours => "Open tours",
            Entry::Save => "Save",
            Entry::Export => "Export…",
            Entry::PublishTour => "Publish tour…",
            Entry::CloseWindow => "Close window",
            Entry::Quit => "Quit Mythicshot",
            Entry::Undo => "Undo",
            Entry::Redo => "Redo",
            Entry::Copy => "Copy",
            Entry::Delete => "Delete",
            Entry::Properties => "Properties",
            Entry::Preferences(tab) => tab.label(),
            Entry::Workspace => Page::Workspace.label(),
            Entry::Troubleshooting => "Troubleshooting",
            Entry::LogFolder => "Open the log folder",
            Entry::DownloadPage => "Download page",
            Entry::About => "About Mythicshot",
        }
    }

    /// What the app does for this entry, the same in every window; None for the entries each
    /// window runs itself (its own save, undo, close and the like).
    pub fn app_request(self) -> Option<AppRequest> {
        Some(match self {
            Entry::CaptureRegion => AppRequest::Run(Command::from_action(Action::CaptureRegion)),
            Entry::RecordRegion => AppRequest::Run(Command::from_action(Action::RecordRegion)),
            Entry::Quit => AppRequest::Run(Command::Exit),
            Entry::OpenTours => AppRequest::Page(Page::Tours),
            Entry::Workspace => AppRequest::Page(Page::Workspace),
            Entry::Preferences(tab) => AppRequest::Preferences(tab),
            // Preferences › General is where the version is shown.
            Entry::About => AppRequest::Preferences(Tab::General),
            Entry::Troubleshooting => AppRequest::Troubleshooting,
            Entry::LogFolder => AppRequest::LogFolder,
            Entry::DownloadPage => AppRequest::DownloadPage,
            Entry::Save | Entry::Export | Entry::PublishTour | Entry::CloseWindow | Entry::Undo | Entry::Redo | Entry::Copy | Entry::Delete | Entry::Properties => return None,
        })
    }
}

/// What a window asks the app to do for a menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRequest {
    /// Run a command as its hotkey or the tray menu does.
    Run(Command),
    /// Show the main window on this page.
    Page(Page),
    /// Show the main window on Preferences, at this section.
    Preferences(Tab),
    /// Open the README's Troubleshooting help: the README installed beside the program.
    Troubleshooting,
    /// Open the folder that holds `crash.log` and `capture.log`.
    LogFolder,
    /// Open [`DOWNLOAD_PAGE`] in the browser.
    DownloadPage,
}

/// What the app says when "Troubleshooting" finds no README beside the program (a build that was
/// not installed).
pub const NO_README: &str = "The README with the troubleshooting help is not beside Mythicshot.";

/// The README installed beside the program at `exe`, which holds the Troubleshooting help.
pub fn readme_beside(exe: &std::path::Path) -> std::path::PathBuf {
    exe.with_file_name("README.md")
}

/// What applies in a window now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// A capture or recording is going on: Capture region and Record region are greyed out, as
    /// the main window's tiles are.
    pub busy: bool,
    pub save: bool,
    pub export: bool,
    pub publish: bool,
    pub undo: bool,
    pub redo: bool,
    pub copy: bool,
    pub delete: bool,
    /// The Properties panel: None where the window has none (the main window), else whether it
    /// can be used here and whether it is open.
    pub properties: Option<(bool, bool)>,
    /// The shortcut text beside each entry, empty for none.
    pub keys: Keys,
}

/// The bindings the menus show beside their entries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keys {
    pub capture_region: String,
    pub record_region: String,
    pub save: String,
    pub undo: String,
    pub redo: String,
    pub copy: String,
    pub delete: String,
}

impl Keys {
    /// From the global bindings and the editor's own.
    pub fn from(global: impl Fn(Action) -> String, editor: impl Fn(EditorAction) -> String) -> Self {
        let show = |b: String| hotkeys::display(&b);
        Keys {
            capture_region: show(global(Action::CaptureRegion)),
            record_region: show(global(Action::RecordRegion)),
            save: show(editor(EditorAction::Save)),
            undo: show(editor(EditorAction::Undo)),
            redo: show(editor(EditorAction::Redo)),
            copy: show(editor(EditorAction::Copy)),
            delete: show(editor(EditorAction::Delete)),
        }
    }
}

/// One line of a menu as it is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub entry: Entry,
    pub enabled: bool,
    /// Some for an entry with a check mark: whether it is checked.
    pub checked: Option<bool>,
    pub shortcut: String,
}

/// A menu: its title and its entries in groups, a line between groups.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub title: &'static str,
    pub groups: Vec<Vec<Item>>,
}

impl Menu {
    #[cfg(test)]
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.groups.iter().flatten()
    }
}

fn item(entry: Entry, enabled: bool, shortcut: &str) -> Item {
    Item { entry, enabled, checked: None, shortcut: shortcut.to_string() }
}

/// The four menus for a window in `state`.
pub fn menus(state: &State) -> Vec<Menu> {
    let k = &state.keys;
    let file = vec![
        vec![item(Entry::CaptureRegion, !state.busy, &k.capture_region), item(Entry::RecordRegion, !state.busy, &k.record_region)],
        vec![item(Entry::OpenTours, true, "")],
        vec![item(Entry::Save, state.save, &k.save), item(Entry::Export, state.export, ""), item(Entry::PublishTour, state.publish, "")],
        vec![item(Entry::CloseWindow, true, ""), item(Entry::Quit, true, "")],
    ];
    let mut edit = vec![
        vec![item(Entry::Undo, state.undo, &k.undo), item(Entry::Redo, state.redo, &k.redo)],
        vec![item(Entry::Copy, state.copy, &k.copy), item(Entry::Delete, state.delete, &k.delete)],
    ];
    if let Some((enabled, open)) = state.properties {
        edit.push(vec![Item { checked: Some(open), ..item(Entry::Properties, enabled, "") }]);
    }
    let prefs = vec![Tab::ALL.iter().map(|t| item(Entry::Preferences(*t), true, "")).collect(), vec![item(Entry::Workspace, true, "")]];
    let help = vec![vec![item(Entry::Troubleshooting, true, ""), item(Entry::LogFolder, true, ""), item(Entry::DownloadPage, true, "")], vec![item(Entry::About, true, "")]];
    TITLES.into_iter().zip([file, edit, prefs, help]).map(|(title, groups)| Menu { title, groups }).collect()
}

/// What the menu bar did in a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarOut {
    /// The entry clicked.
    pub picked: Option<Entry>,
    /// One of its menus is open: Esc is the menu's, to close it, in the next frame.
    pub open: bool,
    /// Where the bar is, so a press on it is not taken as a press elsewhere in the window.
    pub rect: egui::Rect,
}

/// The menu bar across the top of a window, above everything else in it: a panel of its own on
/// surface.
pub fn bar(ui: &mut egui::Ui, id: &'static str, state: &State) -> BarOut {
    let frame = egui::Frame::new().fill(theme::SURFACE).inner_margin(egui::Margin::symmetric(4, 1));
    let (mut picked, mut open) = (None, false);
    let rect = egui::Panel::top(id).resizable(false).frame(frame).show(ui, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            for menu in menus(state) {
                open |= ui.menu_button(menu.title, |ui| {
                    ui.set_min_width(200.0);
                    for (i, group) in menu.groups.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        for it in group {
                            if entry_ui(ui, it) {
                                picked = Some(it.entry);
                                ui.close();
                            }
                        }
                    }
                })
                .inner
                .is_some();
            }
        });
    });
    BarOut { picked, open: open && picked.is_none(), rect: rect.response.rect }
}

/// One entry: a button with its shortcut on the right, or a check box; disabled when it does
/// not apply. True when it was clicked.
fn entry_ui(ui: &mut egui::Ui, it: &Item) -> bool {
    let resp = match it.checked {
        Some(checked) => {
            let mut shown = checked;
            ui.add_enabled(it.enabled, egui::Checkbox::new(&mut shown, it.entry.label()))
        }
        None => ui.add_enabled(it.enabled, egui::Button::new(it.entry.label()).shortcut_text(it.shortcut.as_str())),
    };
    let resp = if it.entry == Entry::Properties { resp.on_disabled_hover_text(NOTHING_TO_SET) } else { resp };
    resp.clicked()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The labels of `menu`, group by group.
    pub(crate) fn labels(menu: &Menu) -> Vec<Vec<&'static str>> {
        menu.groups.iter().map(|g| g.iter().map(|i| i.entry.label()).collect()).collect()
    }

    /// Where `text` was drawn last in the frame: a popup is painted after the window under it, so
    /// an entry of an open menu is found before a word of the same name in the window.
    pub(crate) fn last_text_rect(out: &egui::FullOutput, text: &str) -> Option<egui::Rect> {
        crate::share_ui::tests::shapes_of(out).into_iter().rev().find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == text => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        })
    }

    /// A left click at `at` as a mouse makes it: the press in one frame, the release in the next,
    /// and one frame more for what it changed to show.
    pub(crate) fn click_at(frame: &mut dyn FnMut(Vec<egui::Event>) -> egui::FullOutput, at: egui::Pos2) -> egui::FullOutput {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(vec![egui::Event::PointerMoved(at), button(true)]);
        frame(vec![button(false)]);
        frame(vec![])
    }

    /// Open the menu titled `menu` with a click on its title; the frame that shows it open.
    pub(crate) fn open_menu(frame: &mut dyn FnMut(Vec<egui::Event>) -> egui::FullOutput, menu: &str) -> egui::FullOutput {
        let out = frame(vec![]);
        let title = crate::share_ui::tests::text_rect(&out, menu).unwrap_or_else(|| panic!("the menu bar shows {menu}"));
        assert!(title.top() < 30.0, "{menu} is at the very top of the window: {title:?}");
        click_at(frame, title.center());
        frame(vec![])
    }

    /// Open `menu` and click its entry `entry`, as a user picks it.
    pub(crate) fn choose(frame: &mut dyn FnMut(Vec<egui::Event>) -> egui::FullOutput, menu: &str, entry: &str) {
        let out = open_menu(frame, menu);
        let at = last_text_rect(&out, entry).unwrap_or_else(|| panic!("{menu} shows {entry}")).center();
        click_at(frame, at);
        // The menu closed with the click, as menus do.
        let after = frame(vec![]);
        let open = last_text_rect(&after, entry).is_some_and(|r| (r.center() - at).length() < 1.0);
        assert!(!open, "{menu} closed after {entry} was chosen");
    }

    /// Every text the frame drew lies inside `rect` (the window, a toolbar), checked by its letters' bounds.
    pub(crate) fn assert_inside(out: &egui::FullOutput, rect: egui::Rect, what: &str) {
        for sh in crate::share_ui::tests::shapes_of(out) {
            if let egui::Shape::Text(t) = sh {
                let r = t.visual_bounding_rect();
                assert!(rect.expand(0.5).contains_rect(r), "{what}: {:?} at {r:?} is not inside {rect:?}", t.galley.text());
            }
        }
    }

    /// An open menu fits: its popup lies inside `window`, and every entry's label and shortcut
    /// lie inside the popup, with the shortcut right of its label on the same row.
    pub(crate) fn assert_menu_fits(out: &egui::FullOutput, menu: &Menu, window: egui::Rect, what: &str) {
        let first = menu.groups[0][0].entry.label();
        let at = last_text_rect(out, first).unwrap_or_else(|| panic!("{what}: {first} is shown")).center();
        // The popup's frame: surface-raised with a popover's corners, the smallest around the first entry.
        let popup = crate::theme::checks::fills(out).into_iter().filter(|(r, c, radius)| *c == theme::RAISED && *radius == theme::RADIUS_LG && r.contains(at)).map(|(r, ..)| r).min_by(|a, b| a.area().total_cmp(&b.area()));
        let popup = popup.unwrap_or_else(|| panic!("{what}: the menu's popup"));
        assert!(window.expand(0.5).contains_rect(popup), "{what}: the menu {popup:?} leaves the window {window:?}");
        // The texts drawn in the popup: over it, so after its frame.
        let shapes = crate::share_ui::tests::shapes_of(out);
        let frame_at = shapes.iter().rposition(|s| matches!(s, egui::Shape::Rect(r) if r.fill == theme::RAISED && popup.contains_rect(r.rect) && r.rect.area() >= popup.area() - 1.0)).expect("the popup's frame");
        let inside: Vec<(String, egui::Rect)> = shapes[frame_at..]
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
                _ => None,
            })
            .filter(|(_, r)| popup.contains(r.center()))
            .collect();
        for it in menu.items() {
            // The label is the leftmost text of its words (Delete's shortcut is "Delete" too).
            let label = inside.iter().filter(|(t, _)| t == it.entry.label()).map(|(_, r)| *r).min_by(|a, b| a.left().total_cmp(&b.left()));
            let label = label.unwrap_or_else(|| panic!("{what}: {:?} is shown in the menu", it.entry));
            assert!(popup.expand(0.5).contains_rect(label), "{what}: {:?} at {label:?} sticks out of the menu {popup:?}", it.entry.label());
            if !it.shortcut.is_empty() {
                let keys = inside.iter().filter(|(t, r)| *t == it.shortcut && (r.center().y - label.center().y).abs() < 2.0 && r.left() > label.left()).map(|(_, r)| *r).next();
                let keys = keys.unwrap_or_else(|| panic!("{what}: {} beside {:?}", it.shortcut, it.entry));
                assert!(popup.expand(0.5).contains_rect(keys) && keys.left() > label.right(), "{what}: {} at {keys:?} beside {label:?} in {popup:?}", it.shortcut);
            }
        }
    }

    pub(crate) fn find(menus: &[Menu], entry: Entry) -> Item {
        menus.iter().flat_map(|m| m.items()).find(|i| i.entry == entry).cloned().unwrap_or_else(|| panic!("{entry:?} is in the menus"))
    }

    #[test]
    fn the_four_menus_hold_exactly_these_entries_in_this_order() {
        let all = menus(&State { properties: Some((true, false)), ..Default::default() });
        assert_eq!(all.iter().map(|m| m.title).collect::<Vec<_>>(), ["File", "Edit", "Preferences", "Help"]);
        assert_eq!(labels(&all[0]), vec![vec!["Capture region", "Record region"], vec!["Open tours"], vec!["Save", "Export…", "Publish tour…"], vec!["Close window", "Quit Mythicshot"]]);
        assert_eq!(labels(&all[1]), vec![vec!["Undo", "Redo"], vec!["Copy", "Delete"], vec!["Properties"]]);
        // The Preferences page's own section names, then the page that holds the sign-in.
        assert_eq!(labels(&all[2]), vec![vec!["General", "Output", "Hotkeys"], vec!["Workspace"]]);
        assert_eq!(labels(&all[3]), vec![vec!["Troubleshooting", "Open the log folder", "Download page"], vec!["About Mythicshot"]]);
        // A window without the panel has no Properties entry.
        let main = menus(&State::default());
        assert_eq!(labels(&main[1]), vec![vec!["Undo", "Redo"], vec!["Copy", "Delete"]]);
    }

    #[test]
    fn entries_that_do_not_apply_are_disabled_not_hidden() {
        let none = menus(&State { properties: Some((false, false)), ..Default::default() });
        for e in [Entry::Save, Entry::Export, Entry::PublishTour, Entry::Undo, Entry::Redo, Entry::Copy, Entry::Delete, Entry::Properties] {
            assert!(!find(&none, e).enabled, "{e:?}");
        }
        // What is always there: capture, record, the pages, closing and quitting, and the help.
        for e in [Entry::CaptureRegion, Entry::RecordRegion, Entry::OpenTours, Entry::CloseWindow, Entry::Quit, Entry::Workspace, Entry::Troubleshooting, Entry::LogFolder, Entry::DownloadPage, Entry::About] {
            assert!(find(&none, e).enabled, "{e:?}");
        }
        for t in Tab::ALL {
            assert!(find(&none, Entry::Preferences(t)).enabled);
        }
        let busy = menus(&State { busy: true, ..Default::default() });
        assert!(!find(&busy, Entry::CaptureRegion).enabled && !find(&busy, Entry::RecordRegion).enabled, "greyed out while something is captured");
        assert!(find(&busy, Entry::OpenTours).enabled && find(&busy, Entry::Quit).enabled);
        let all = menus(&State { busy: false, save: true, export: true, publish: true, undo: true, redo: true, copy: true, delete: true, properties: Some((true, true)), keys: Keys::default() });
        assert!(all.iter().flat_map(|m| m.items()).all(|i| i.enabled));
        assert_eq!(find(&all, Entry::Properties).checked, Some(true), "checked while the panel is open");
        assert_eq!(find(&menus(&State { properties: Some((true, false)), ..Default::default() }), Entry::Properties).checked, Some(false));
        assert!(all.iter().flat_map(|m| m.items()).filter(|i| i.entry != Entry::Properties).all(|i| i.checked.is_none()));
    }

    #[test]
    fn entries_show_the_apps_own_shortcuts() {
        let defaults = Keys::from(|a| a.default_binding().to_string(), |a| a.default_binding().to_string());
        let all = menus(&State { keys: defaults, ..Default::default() });
        for (e, keys) in [(Entry::CaptureRegion, "PrtSc"), (Entry::RecordRegion, "Shift+PrtSc"), (Entry::Save, "Ctrl+S"), (Entry::Undo, "Ctrl+Z"), (Entry::Redo, "Ctrl+Y"), (Entry::Copy, "Ctrl+C"), (Entry::Delete, "Delete")] {
            assert_eq!(find(&all, e).shortcut, keys, "{e:?}");
        }
        // A shortcut the user cleared shows nothing, and every entry without one shows nothing.
        let none = Keys::from(|_| String::new(), |_| String::new());
        assert!(menus(&State { keys: none, ..Default::default() }).iter().flat_map(|m| m.items()).all(|i| i.shortcut.is_empty()));
        for e in [Entry::OpenTours, Entry::Export, Entry::PublishTour, Entry::CloseWindow, Entry::Quit, Entry::About] {
            assert!(find(&all, e).shortcut.is_empty(), "{e:?}");
        }
    }

    #[test]
    fn app_entries_run_what_the_hotkeys_and_the_tray_run() {
        // The same commands as the Capture region and Record region hotkeys and the tray's Exit.
        assert_eq!(Entry::CaptureRegion.app_request(), Some(AppRequest::Run(Command::Region)));
        assert_eq!(Entry::RecordRegion.app_request(), Some(AppRequest::Run(Command::Record)));
        assert_eq!(Entry::Quit.app_request(), Some(AppRequest::Run(Command::Exit)));
        assert_eq!(Entry::OpenTours.app_request(), Some(AppRequest::Page(Page::Tours)));
        assert_eq!(Entry::Workspace.app_request(), Some(AppRequest::Page(Page::Workspace)));
        for t in Tab::ALL {
            assert_eq!(Entry::Preferences(t).app_request(), Some(AppRequest::Preferences(t)));
        }
        assert_eq!(Entry::About.app_request(), Some(AppRequest::Preferences(Tab::General)), "where the version is shown");
        assert_eq!(Entry::Troubleshooting.app_request(), Some(AppRequest::Troubleshooting));
        assert_eq!(Entry::LogFolder.app_request(), Some(AppRequest::LogFolder));
        assert_eq!(Entry::DownloadPage.app_request(), Some(AppRequest::DownloadPage));
        for e in [Entry::Save, Entry::Export, Entry::PublishTour, Entry::CloseWindow, Entry::Undo, Entry::Redo, Entry::Copy, Entry::Delete, Entry::Properties] {
            assert_eq!(e.app_request(), None, "{e:?} is the window's own");
        }
        assert_eq!(DOWNLOAD_PAGE, "https://mythicshot.com/download/");
        assert!(mythicshot_destinations::is_web_link(DOWNLOAD_PAGE), "a link the browser is asked to open");
    }

    #[test]
    fn the_readme_is_looked_for_beside_the_program() {
        let exe = std::path::Path::new(r"C:\Program Files\Mythicshot\mythicshot.exe");
        assert_eq!(readme_beside(exe), std::path::Path::new(r"C:\Program Files\Mythicshot\README.md"));
    }
}
