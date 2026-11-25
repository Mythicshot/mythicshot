//! The main window's Home page: the capture tiles and the newest files in the captures folder.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align, Align2, CornerRadius, FontId, Layout, RichText, Sense, Stroke, StrokeKind, pos2, vec2};

use crate::hotkeys::{self, Action};
use crate::icons::{self, Icon};
use crate::main_window::PageOut;
use crate::platform;
use crate::settings::Settings;
use crate::theme;
use crate::tray::Command;


/// How many files "Recent" lists.
pub const RECENT_LIMIT: usize = 5;
/// How often the captures folder is read again while the page is open.
const REFRESH_EVERY: Duration = Duration::from_secs(2);
/// A read that has not come back after this long is given up on (a share that went away, a drive
/// that will not wake); the page says the folder cannot be read and a later read may start.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait before reading a folder again that could not be read.
const RETRY_EVERY: Duration = Duration::from_secs(30);
/// How many reads of one folder may be given up on. Each leaves a thread waiting on the drive, so
/// after this many the folder is not tried again until the captures folder setting changes.
const MAX_ABANDONED: u32 = 3;

pub const RECORD_A_TOUR: &str = "Record a tour: record a region or window, then choose Tour view in the editor.";
pub const NOTHING_RECENT: &str = "Nothing is saved in the captures folder yet.";
pub const READING_RECENT: &str = "Reading the captures folder…";
pub const CANNOT_READ_RECENT: &str = "The captures folder cannot be read right now.";

/// A capture tile: what it is called, the action whose hotkey it shows, and the command it runs,
/// the same one the tray menu and the hotkey run.
pub struct Tile {
    pub name: &'static str,
    pub icon: Icon,
    pub action: Action,
    pub command: Command,
}

pub const TILES: [Tile; 4] = [
    Tile { name: "Region", icon: Icon::Region, action: Action::CaptureRegion, command: Command::Region },
    Tile { name: "Window", icon: Icon::Window, action: Action::CaptureWindow, command: Command::Window },
    Tile { name: "Full screen", icon: Icon::Screen, action: Action::CaptureFullScreen, command: Command::FullScreen },
    Tile { name: "Record", icon: Icon::Record, action: Action::RecordRegion, command: Command::Record },
];

/// The hotkey a tile shows: the one set for its action now, empty when it has none.
pub fn tile_hotkey(settings: &Settings, action: Action) -> String {
    hotkeys::display(settings.binding(action))
}

/// A file in the captures folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recent {
    pub path: PathBuf,
    pub name: String,
}

/// Whether a file name belongs in "Recent". Left out: hidden files, which is where the app keeps a
/// tour or site page it is still writing (`.mythicshot-….tmp`), any other `.tmp` or `.part` file,
/// and the two files Windows adds to folders by itself.
fn listed(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !(name.starts_with('.') || lower.ends_with(".tmp") || lower.ends_with(".part") || lower == "desktop.ini" || lower == "thumbs.db")
}

/// The folder exists but could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unreadable;

/// The newest files directly in `dir`, newest first, at most `limit`. Folders are left out, and so
/// are the files [`listed`] names and the files in `exporting`, which the app is still writing.
/// A folder that does not exist yet has nothing in it; one that cannot be read is an error.
pub fn recent_files(dir: &Path, limit: usize, exporting: &[PathBuf]) -> Result<Vec<Recent>, Unreadable> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(Unreadable),
    };
    let mut files: Vec<(SystemTime, Recent)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            if !meta.is_file() || !listed(&name) || exporting.iter().any(|x| same_file(x, &path)) {
                return None;
            }
            Some((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), Recent { path, name }))
        })
        .collect();
    // Newest first; files saved in the same instant stay in name order.
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    files.truncate(limit);
    Ok(files.into_iter().map(|(_, f)| f).collect())
}

/// Whether two paths name the same file, however their separators and letter case are written.
fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    a == b || norm(a) == norm(b)
}

/// What "Recent" shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecentView {
    /// The first read of this folder has not come back yet.
    Reading,
    /// The folder could not be read, or a read of it never came back.
    Unreadable,
    Empty,
    Files(Vec<Recent>),
}

/// The last listing of the captures folder and the read in progress, as plain state: `poll` says
/// when a read should start, `finish` takes its result. A read that never comes back is given up
/// on after [`READ_TIMEOUT`], so it cannot block every later one.
#[derive(Default)]
struct Listing {
    dir: PathBuf,
    result: Option<Result<Vec<Recent>, Unreadable>>,
    read_at: Option<Instant>,
    /// The read in progress: its number and when it started.
    reading: Option<(u64, Instant)>,
    last_read: u64,
    abandoned: u32,
}

impl Listing {
    /// Called every frame the page is drawn. Returns the number of a read to start now, if one should.
    fn poll(&mut self, dir: &Path, now: Instant) -> Option<u64> {
        if self.dir != dir {
            // Another folder: forget the old one's files; a read of it still running is ignored.
            *self = Listing { dir: dir.to_path_buf(), last_read: self.last_read, ..Listing::default() };
        }
        if let Some((_, started)) = self.reading {
            if now.saturating_duration_since(started) < READ_TIMEOUT {
                return None;
            }
            self.reading = None;
            self.abandoned += 1;
            self.result = Some(Err(Unreadable));
            self.read_at = Some(now);
        }
        let unreadable = matches!(self.result, Some(Err(Unreadable)));
        if unreadable && self.abandoned >= MAX_ABANDONED {
            return None;
        }
        let wait = if unreadable { RETRY_EVERY } else { REFRESH_EVERY };
        if self.read_at.is_some_and(|t| now.saturating_duration_since(t) < wait) {
            return None;
        }
        self.last_read += 1;
        self.reading = Some((self.last_read, now));
        Some(self.last_read)
    }

    /// The result of read number `read`. One that was given up on, or was of another folder, is ignored.
    fn finish(&mut self, read: u64, result: Result<Vec<Recent>, Unreadable>, now: Instant) {
        if self.reading.is_some_and(|(n, _)| n == read) {
            self.reading = None;
            self.result = Some(result);
            self.read_at = Some(now);
        }
    }

    fn view(&self) -> RecentView {
        match &self.result {
            None => RecentView::Reading,
            Some(Err(Unreadable)) => RecentView::Unreadable,
            Some(Ok(files)) if files.is_empty() => RecentView::Empty,
            Some(Ok(files)) => RecentView::Files(files.clone()),
        }
    }
}

/// The Home page's state: the last listing of the captures folder. The folder is read on a
/// background thread, so a slow or sleeping drive never freezes the window.
#[derive(Default)]
pub struct HomeState {
    listing: Arc<Mutex<Listing>>,
}

impl HomeState {
    /// What to show for `dir`, starting a fresh read when the last one is old or was of another folder.
    pub fn recent(&self, dir: &Path) -> RecentView {
        let Ok(mut l) = self.listing.lock() else { return RecentView::Unreadable };
        if let Some(read) = l.poll(dir, Instant::now()) {
            let (listing, dir) = (self.listing.clone(), dir.to_path_buf());
            std::thread::spawn(move || {
                let result = recent_files(&dir, RECENT_LIMIT, &crate::recorder_ui::exporting_paths());
                if let Ok(mut l) = listing.lock() {
                    l.finish(read, result, Instant::now());
                }
            });
        }
        l.view()
    }
}

/// Draw the page. `busy` greys the tiles out while a capture or recording is already going on.
pub fn ui(ui: &mut egui::Ui, state: &mut HomeState, settings: &Settings, busy: bool, out: &mut PageOut) {
    ui.heading("Capture");
    let gap = theme::ITEM_GAP;
    let width = ((ui.available_width() - gap * (TILES.len() - 1) as f32) / TILES.len() as f32).max(96.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for t in &TILES {
            if tile(ui, t, &tile_hotkey(settings, t.action), width, !busy) {
                out.capture = Some(t.command);
            }
        }
    });
    theme::caption(ui, RECORD_A_TOUR);
    theme::block_gap(ui);
    ui.heading("Recent");
    match state.recent(&settings.output_dir) {
        RecentView::Reading => {
            ui.label(RichText::new(READING_RECENT).color(theme::MUTED));
        }
        RecentView::Unreadable => {
            ui.label(RichText::new(CANNOT_READ_RECENT).color(theme::MUTED));
        }
        RecentView::Empty => {
            ui.label(RichText::new(NOTHING_RECENT).color(theme::MUTED));
        }
        RecentView::Files(files) => {
            egui::ScrollArea::vertical().show(ui, |ui| {
                theme::card(ui, |rows| {
                    for f in &files {
                        rows.full(|ui| recent_row(ui, f));
                    }
                });
            });
        }
    }
    // New captures show up without a click.
    ui.ctx().request_repaint_after(REFRESH_EVERY);
}

/// One capture tile: its icon, its name and its hotkey. Returns whether it was clicked.
fn tile(ui: &mut egui::Ui, t: &Tile, hotkey: &str, width: f32, enabled: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 112.0), if enabled { Sense::click() } else { Sense::hover() });
    let hovered = enabled && resp.hovered();
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(theme::RADIUS_LG), if hovered { theme::RAISED } else { theme::SURFACE }, Stroke::new(1.0, theme::BORDER_STRONG), StrokeKind::Inside);
    let (fg, dim) = if enabled { (theme::TEXT, theme::MUTED) } else { (theme::DISABLED, theme::DISABLED) };
    icons::paint(p, egui::Rect::from_center_size(pos2(rect.center().x, rect.top() + 34.0), vec2(28.0, 28.0)), t.icon, fg);
    p.text(pos2(rect.center().x, rect.top() + 70.0), Align2::CENTER_CENTER, t.name, FontId::proportional(14.0), fg);
    if !hotkey.is_empty() {
        p.text(pos2(rect.center().x, rect.top() + 92.0), Align2::CENTER_CENTER, hotkey, FontId::monospace(12.0), dim);
    }
    resp.clicked()
}

/// One recent capture, a row of the Recent card: its name, then Open and Show in folder at the right.
fn recent_row(ui: &mut egui::Ui, f: &Recent) {
    ui.horizontal(|ui| {
        theme::value_then_actions(
            ui,
            ("recent", &f.path),
            Layout::left_to_right(Align::Center),
            |ui| drop(theme::value(ui, &f.name)),
            |ui| {
                if ui.button("Open").clicked() {
                    platform::open_path(&f.path);
                }
                if ui.button("Show in folder").clicked() {
                    platform::reveal(&f.path);
                }
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `name` in `dir`, last changed `age` seconds before now.
    fn file(dir: &Path, name: &str, age: u64) {
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age)).unwrap();
    }

    fn names(dir: &Path, limit: usize) -> Vec<String> {
        recent_files(dir, limit, &[]).unwrap().into_iter().map(|f| f.name).collect()
    }

    #[test]
    fn the_newest_five_files_come_first() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, age) in [("old.png", 700), ("b.gif", 200), ("a.png", 100), ("tour.html", 300), ("c.jpg", 400), ("d.mp4", 500), ("e.png", 600)] {
            file(tmp.path(), name, age);
        }
        assert_eq!(names(tmp.path(), RECENT_LIMIT), ["a.png", "b.gif", "tour.html", "c.jpg", "d.mp4"]);
        assert_eq!(names(tmp.path(), 2), ["a.png", "b.gif"]);
        assert_eq!(names(tmp.path(), 0), Vec::<String>::new());
        // Each one carries its full path, for Open and Show in folder.
        assert_eq!(recent_files(tmp.path(), 1, &[]).unwrap()[0].path, tmp.path().join("a.png"));
    }

    #[test]
    fn folders_and_files_still_being_written_are_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "shot.png", 50);
        // What the app writes while a tour or a site page is being saved, and other leftovers.
        for name in [".mythicshot-tour-a1B2c3.tmp", ".mythicshot-Zz9Yy8.tmp", "export.TMP", "movie.gif.part", "desktop.ini", "Thumbs.db", ".hidden"] {
            file(tmp.path(), name, 1);
        }
        // A tour bundle is a folder, and so is a subfolder from the file name pattern.
        std::fs::create_dir(tmp.path().join("Billing tour")).unwrap();
        std::fs::create_dir(tmp.path().join("2026")).unwrap();
        file(&tmp.path().join("2026"), "inside.png", 1);
        assert_eq!(names(tmp.path(), RECENT_LIMIT), ["shot.png"]);
    }

    #[test]
    fn a_missing_or_empty_folder_lists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(recent_files(tmp.path(), RECENT_LIMIT, &[]), Ok(vec![]));
        assert_eq!(recent_files(&tmp.path().join("never made"), RECENT_LIMIT, &[]), Ok(vec![]));
        // Looking never creates the folder.
        assert!(!tmp.path().join("never made").exists());
    }

    #[test]
    fn a_folder_that_cannot_be_read_is_not_called_empty() {
        let tmp = tempfile::tempdir().unwrap();
        // The setting names something that is there but is not a folder.
        file(tmp.path(), "not a folder", 5);
        assert_eq!(recent_files(&tmp.path().join("not a folder"), RECENT_LIMIT, &[]), Err(Unreadable));
    }

    #[test]
    fn a_file_still_being_exported_is_left_out_until_it_is_done() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "done.gif", 100);
        file(tmp.path(), "Half Written.gif", 1);
        let exporting = [tmp.path().join("Half Written.gif")];
        let listed = |x: &[PathBuf]| recent_files(tmp.path(), RECENT_LIMIT, x).unwrap().into_iter().map(|f| f.name).collect::<Vec<_>>();
        assert_eq!(listed(&exporting), ["done.gif"]);
        // The same path written another way is the same file.
        let other = PathBuf::from(tmp.path().join("half written.GIF").to_string_lossy().replace('\\', "/"));
        assert_eq!(listed(&[other]), ["done.gif"]);
        // Once the export has ended it is listed, newest first.
        assert_eq!(listed(&[]), ["Half Written.gif", "done.gif"]);
        // An export elsewhere with the same file name hides nothing here.
        assert_eq!(listed(&[PathBuf::from("Z:/elsewhere/done.gif")]), ["Half Written.gif", "done.gif"]);
    }

    fn recent(name: &str) -> Recent {
        Recent { path: PathBuf::from(name), name: name.into() }
    }

    #[test]
    fn the_page_says_reading_then_what_the_read_found() {
        let (dir, t0) = (Path::new("C:/captures"), Instant::now());
        let mut l = Listing::default();
        let read = l.poll(dir, t0).expect("the first look starts a read");
        assert_eq!(l.view(), RecentView::Reading);
        assert_eq!(l.poll(dir, t0 + Duration::from_secs(1)), None, "one read at a time");
        l.finish(read, Ok(vec![]), t0 + Duration::from_secs(1));
        assert_eq!(l.view(), RecentView::Empty);
        // Read again after the refresh time; the last answer stays up meanwhile.
        assert_eq!(l.poll(dir, t0 + Duration::from_secs(2)), None);
        let read = l.poll(dir, t0 + Duration::from_secs(1) + REFRESH_EVERY).expect("a refresh");
        assert_eq!(l.view(), RecentView::Empty);
        l.finish(read, Ok(vec![recent("a.png")]), t0 + Duration::from_secs(4));
        assert_eq!(l.view(), RecentView::Files(vec![recent("a.png")]));
        // A folder that cannot be read says so, and is tried again later, not every two seconds.
        let read = l.poll(dir, t0 + Duration::from_secs(10)).unwrap();
        l.finish(read, Err(Unreadable), t0 + Duration::from_secs(10));
        assert_eq!(l.view(), RecentView::Unreadable);
        assert_eq!(l.poll(dir, t0 + Duration::from_secs(10) + REFRESH_EVERY), None);
        assert!(l.poll(dir, t0 + Duration::from_secs(10) + RETRY_EVERY).is_some());
    }

    #[test]
    fn a_read_that_never_comes_back_is_given_up_on_and_a_later_one_can_start() {
        let (dir, t0) = (Path::new("//gone/share"), Instant::now());
        let mut l = Listing::default();
        let hung = l.poll(dir, t0).unwrap();
        assert_eq!(l.poll(dir, t0 + READ_TIMEOUT - Duration::from_millis(1)), None);
        assert_eq!(l.view(), RecentView::Reading);
        // Given up on: the page stops saying "reading".
        assert_eq!(l.poll(dir, t0 + READ_TIMEOUT), None);
        assert_eq!(l.view(), RecentView::Unreadable);
        // A later read starts, and its answer is shown.
        let retry = l.poll(dir, t0 + READ_TIMEOUT + RETRY_EVERY).expect("a new read after the wait");
        assert_ne!(retry, hung);
        // The hung read coming back late changes nothing.
        l.finish(hung, Ok(vec![recent("stale.png")]), t0 + READ_TIMEOUT + RETRY_EVERY);
        assert_eq!(l.view(), RecentView::Unreadable);
        l.finish(retry, Ok(vec![recent("a.png")]), t0 + READ_TIMEOUT + RETRY_EVERY + Duration::from_secs(1));
        assert_eq!(l.view(), RecentView::Files(vec![recent("a.png")]));
    }

    #[test]
    fn a_folder_that_keeps_hanging_is_left_alone_after_a_few_tries() {
        let (dir, mut now) = (Path::new("//gone/share"), Instant::now());
        let mut l = Listing::default();
        let mut started = 0;
        for _ in 0..200 {
            if l.poll(dir, now).is_some() {
                started += 1;
            }
            now += Duration::from_secs(5);
        }
        assert_eq!(started, MAX_ABANDONED as usize, "each hung read leaves a waiting thread");
        assert_eq!(l.view(), RecentView::Unreadable);
        // Pointing the setting at another folder starts over, and the old folder's reads are ignored.
        let read = l.poll(Path::new("C:/captures"), now).expect("another folder is read at once");
        assert_eq!(l.view(), RecentView::Reading);
        l.finish(read - 1, Ok(vec![recent("old.png")]), now);
        assert_eq!(l.view(), RecentView::Reading);
        l.finish(read, Ok(vec![]), now);
        assert_eq!(l.view(), RecentView::Empty);
    }

    #[test]
    fn files_saved_in_the_same_instant_keep_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        let at = SystemTime::now() - Duration::from_secs(60);
        for name in ["b.png", "a.png", "c.png"] {
            let path = tmp.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_modified(at).unwrap();
        }
        assert_eq!(names(tmp.path(), RECENT_LIMIT), ["a.png", "b.png", "c.png"]);
    }

    #[test]
    fn each_tile_shows_the_hotkey_set_for_its_action() {
        let mut s = Settings::default();
        s.hotkeys.insert(Action::CaptureRegion, "PrintScreen".into());
        s.hotkeys.insert(Action::CaptureWindow, "alt+PrintScreen".into());
        s.hotkeys.insert(Action::CaptureFullScreen, String::new());
        s.hotkeys.insert(Action::RecordRegion, "control+shift+KeyR".into());
        let shown: Vec<(&str, String)> = TILES.iter().map(|t| (t.name, tile_hotkey(&s, t.action))).collect();
        assert_eq!(shown, [("Region", "PrtSc".to_string()), ("Window", "Alt+PrtSc".to_string()), ("Full screen", String::new()), ("Record", "Ctrl+Shift+R".to_string())]);
        // An action missing from the file has no hotkey to show.
        s.hotkeys.remove(&Action::CaptureRegion);
        assert_eq!(tile_hotkey(&s, Action::CaptureRegion), "");
    }

    #[test]
    fn each_tile_runs_the_command_its_hotkey_runs() {
        for t in &TILES {
            assert_eq!(t.command, Command::from_action(t.action), "{}", t.name);
        }
        assert_eq!(TILES.iter().map(|t| t.command).collect::<Vec<_>>(), [Command::Region, Command::Window, Command::FullScreen, Command::Record]);
    }

    #[test]
    fn the_listing_arrives_from_the_background_read() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "shot.png", 5);
        let state = HomeState::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = state.recent(tmp.path());
        assert_eq!(got, RecentView::Reading);
        while got == RecentView::Reading && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            got = state.recent(tmp.path());
        }
        assert_eq!(got, RecentView::Files(vec![Recent { path: tmp.path().join("shot.png"), name: "shot.png".into() }]));
        // Another folder never shows the first one's files.
        let other = tempfile::tempdir().unwrap();
        assert_eq!(state.recent(other.path()), RecentView::Reading);
    }
}
