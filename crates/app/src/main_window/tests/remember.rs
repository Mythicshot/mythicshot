//! The main window opens at the size the user last gave it, fitted to the monitor, and only a
//! resize by hand is saved. The window is driven the way the app drives it ([`show`] for the
//! opening, [`viewport_ui`] for its frames, with the size on screen in its viewport info), and the
//! window's normal size, which the app reads from Windows, comes from a stand-in the test sets.
//! The app's part (taking `size_to_save` into the settings) is done by hand.

use std::sync::{Arc, Mutex};

use eframe::egui;

use super::super::*;
use super::shared;

fn work_1080_at_150() -> Option<egui::Rect> {
    Some(opening::tests::work(1920, 1080, 1.5))
}

fn work_4k_at_150() -> Option<egui::Rect> {
    Some(opening::tests::work(3840, 2160, 1.5))
}

/// The window's normal size as the test says Windows holds it.
struct FakeNormal(Arc<Mutex<Option<egui::Vec2>>>);

impl opening::NormalSize for FakeNormal {
    fn read(&mut self, _: &egui::ViewportInfo, _: f32) -> Option<egui::Vec2> {
        *self.0.lock().unwrap()
    }
}

/// The window, its frame clock, and what the app would hold.
struct Run {
    ctx: egui::Context,
    shared: Arc<Mutex<Shared>>,
    normal: Arc<Mutex<Option<egui::Vec2>>>,
    t: f64,
    _dir: tempfile::TempDir,
}

impl Run {
    fn new(work: fn() -> Option<egui::Rect>, saved: Option<[f32; 2]>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut s = shared(dir.path());
        s.work_area = work;
        s.prefs.settings.main_window_size = saved;
        let normal = Arc::new(Mutex::new(None));
        s.normal_size = Box::new(FakeNormal(normal.clone()));
        let ctx = crate::theme::test_context();
        ctx.set_embed_viewports(false);
        Run { ctx, shared: Arc::new(Mutex::new(s)), normal, t: 0.0, _dir: dir }
    }

    /// One frame of the app with `request`: the window opened or kept, as the app does.
    fn app_frame(&mut self, request: Option<Request>) {
        self.t += 1.0 / 60.0;
        let input = egui::RawInput { time: Some(self.t), ..Default::default() };
        let shared = self.shared.clone();
        let icon = Arc::new(egui::IconData::default());
        self.ctx.run_ui(input, |ui| show(ui.ctx(), &shared, icon.clone(), request)).textures_delta.clear();
        // The app takes a size to save into its settings, which the window's copy then holds.
        let mut s = self.shared.lock().unwrap();
        s.settle(&self.ctx);
        let size = s.size_to_save.take();
        if size_changes(&mut s.prefs.settings, size) {
            SAVES.with(|n| n.borrow_mut().push(size.unwrap()));
        }
    }

    /// The size the open window was opened at.
    fn opened_at(&self) -> egui::Vec2 {
        self.shared.lock().unwrap().opening.expect("the window is open").size
    }

    /// `n` frames of the window's own viewport: `screen` on screen (maximized when the two
    /// differ), and `normal` as its normal size.
    fn window_frames(&mut self, n: usize, screen: egui::Vec2, normal: egui::Vec2, events: Vec<egui::Event>) {
        *self.normal.lock().unwrap() = Some(normal);
        for i in 0..n {
            self.t += 1.0 / 60.0;
            let mut input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)), time: Some(self.t), events: if i == 0 { events.clone() } else { Vec::new() }, ..Default::default() };
            input.viewports.insert(egui::ViewportId::ROOT, egui::ViewportInfo { inner_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)), maximized: Some(screen != normal), minimized: Some(false), ..Default::default() });
            let mut s = self.shared.lock().unwrap();
            let mut out = self.ctx.run_ui(input, |ui| viewport_ui(ui, &mut s));
            out.textures_delta.clear();
            drop(s);
            self.app_frame(None);
        }
    }

    /// Frames of a window in its normal state at `size`.
    fn normal_frames(&mut self, n: usize, size: egui::Vec2) {
        self.window_frames(n, size, size, vec![]);
    }

    fn close(&mut self) {
        self.shared.lock().unwrap().close_window();
        self.app_frame(None);
    }

    fn saved(&self) -> Option<[f32; 2]> {
        self.shared.lock().unwrap().prefs.settings.main_window_size
    }
}

thread_local! {
    static SAVES: std::cell::RefCell<Vec<[f32; 2]>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The saves the app made in this test's thread, taken.
fn take_saves() -> Vec<[f32; 2]> {
    SAVES.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

fn v(size: [f32; 2]) -> egui::Vec2 {
    egui::vec2(size[0], size[1])
}

#[test]
fn a_size_made_by_hand_opens_again_within_the_run_and_after_a_restart() {
    take_saves();
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    assert_eq!(r.opened_at(), v(SIZE), "nothing saved: the default");
    let a = egui::vec2(1100.0, 820.0);
    // Dragged over half a second, then let go.
    for i in 0..30 {
        r.normal_frames(1, v(SIZE) + (a - v(SIZE)) * (i as f32 / 29.0));
    }
    r.normal_frames(60, a);
    assert_eq!(take_saves(), vec![[a.x, a.y]], "saved once, when it settled");
    r.close();
    assert!(take_saves().is_empty(), "nothing more at close");
    // Opened again in the same run.
    r.app_frame(Some(Request::Open));
    assert_eq!(r.opened_at(), a);
    r.normal_frames(30, a);
    r.close();
    assert!(take_saves().is_empty(), "opened and closed untouched: nothing saved");
    // After a restart: the settings file holds it, and a new run opens at it.
    let mut again = Run::new(work_4k_at_150, r.saved());
    again.app_frame(Some(Request::Open));
    assert_eq!(again.opened_at(), a);
}

#[test]
fn a_size_saved_on_a_large_monitor_opens_fitted_on_a_small_one_and_stays_saved() {
    take_saves();
    let big = [1800.0, 1000.0];
    let mut r = Run::new(work_1080_at_150, Some(big));
    r.app_frame(Some(Request::Open));
    let fitted = r.opened_at();
    let work = work_1080_at_150().unwrap();
    assert!(fitted.x < big[0] && fitted.y < big[1] && fitted.x + 16.0 + 16.0 <= work.width() && fitted.y + 39.0 + 16.0 <= work.height() + 0.01, "{fitted:?}");
    // Left at that size, closed: the preference is still the large size.
    r.normal_frames(90, fitted);
    r.close();
    assert_eq!(r.saved(), Some(big));
    assert!(take_saves().is_empty());
    // Resized by hand there: that is the new preference.
    r.app_frame(Some(Request::Open));
    let b = fitted - egui::vec2(100.0, 50.0);
    r.normal_frames(60, b);
    r.close();
    assert_eq!(r.saved(), Some([b.x, b.y]));
    // A saved value that cannot be used: the default.
    for bad in [[f32::NAN, 700.0], [-1.0, 700.0], [0.0, 0.0], [1e12, 1e12]] {
        let mut r = Run::new(work_4k_at_150, Some(bad));
        r.app_frame(Some(Request::Open));
        assert_eq!(r.opened_at(), v(SIZE), "{bad:?}");
    }
}

/// Maximized, the window shows the whole work area while Windows keeps its normal size; that
/// normal size is what is saved and what it reopens at, never maximized.
#[test]
fn a_maximized_window_reopens_at_its_last_normal_size_not_maximized() {
    take_saves();
    let full = egui::vec2(2560.0, 1392.0);
    // Resized to A, maximized, closed: reopens at A, not maximized.
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    let a = egui::vec2(1000.0, 780.0);
    r.normal_frames(60, a);
    r.window_frames(60, full, a, vec![]);
    r.close();
    assert_eq!(r.saved(), Some([a.x, a.y]));
    r.app_frame(Some(Request::Open));
    assert_eq!(r.opened_at(), a);
    let s = r.shared.lock().unwrap();
    let b = builder(Arc::new(egui::IconData::default()), s.opening.unwrap());
    assert_eq!(b.maximized, None, "never opened maximized");
    drop(s);
    r.close();
    // Resized to A and maximized at once, closed maximized before A settled: still A.
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    r.normal_frames(1, a);
    r.window_frames(5, full, a, vec![]);
    r.close();
    assert_eq!(r.saved(), Some([a.x, a.y]));
    // Maximized without ever being resized: reopens at the default, nothing saved.
    take_saves();
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    r.window_frames(60, full, v(SIZE), vec![]);
    r.normal_frames(30, v(SIZE));
    r.window_frames(30, full, v(SIZE), vec![]);
    r.close();
    assert_eq!((r.saved(), take_saves()), (None, vec![]));
    r.app_frame(Some(Request::Open));
    assert_eq!(r.opened_at(), v(SIZE));
}

/// Snapped to half the screen (or given a size by Windows the same way), the window's normal
/// size stays the one from before the snap: that is what it reopens at.
#[test]
fn a_snapped_window_reopens_at_its_size_from_before_the_snap() {
    take_saves();
    let half = egui::vec2(1280.0, 1392.0);
    let a = egui::vec2(1000.0, 780.0);
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    r.normal_frames(60, a);
    r.window_frames(90, half, a, vec![]);
    r.close();
    assert_eq!((r.saved(), take_saves()), (Some([a.x, a.y]), vec![[a.x, a.y]]), "A, saved once, before the snap");
    r.app_frame(Some(Request::Open));
    assert_eq!(r.opened_at(), a);
    r.close();
    // Snapped without a resize before: nothing saved, the default again.
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    r.window_frames(90, half, v(SIZE), vec![]);
    r.close();
    assert_eq!((r.saved(), take_saves()), (None, vec![]));
}

#[test]
fn the_window_opened_only_for_the_popup_after_an_update_saves_nothing() {
    use crate::update::pending::{self, AfterUpdate, Pending};
    take_saves();
    let ver = |s: &str| crate::update::Version::parse(s).unwrap();
    let mut r = Run::new(work_1080_at_150, Some([1800.0, 1000.0]));
    let note = r._dir.path().join(pending::FILE_NAME);
    pending::write(&note, Pending { from: ver("0.2.38"), to: ver("0.2.39") }).unwrap();
    let (request, after) = r.shared.lock().unwrap().start_with_note(None, &note, std::time::SystemTime::now(), ver("0.2.39"));
    assert_eq!((request, after), (None, Some(AfterUpdate::Updated { from: ver("0.2.38"), to: ver("0.2.39") })));
    // The first frame opens it, as any opening: fitted to the monitor.
    r.app_frame(None);
    let at = r.opened_at();
    // Windows reports the size back a pixel off; then OK closes the window opened for it.
    let off = at + egui::vec2(0.7, -0.7);
    r.normal_frames(60, off);
    let enter = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    r.window_frames(1, off, off, vec![enter]);
    assert!(!r.shared.lock().unwrap().nav.is_open(), "OK closed the window opened for the popup");
    r.app_frame(None);
    assert_eq!((r.saved(), take_saves()), (Some([1800.0, 1000.0]), vec![]), "nothing saved by itself");
    assert_eq!(r.shared.lock().unwrap().size_at_exit(), None);
}

/// Quitting the app moments after a resize by hand keeps it: the size the app has not taken yet,
/// or the one that has not settled yet, is what `size_at_exit` hands the app to save on exit.
#[test]
fn quitting_right_after_a_resize_keeps_the_size() {
    take_saves();
    let a = egui::vec2(1000.0, 780.0);
    // Still settling: a quarter of a second after the last change.
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    r.normal_frames(15, a);
    assert!(take_saves().is_empty());
    let mut settings = r.shared.lock().unwrap().prefs.settings.clone();
    let at_exit = r.shared.lock().unwrap().size_at_exit();
    assert_eq!(at_exit, Some([a.x, a.y]));
    assert!(size_changes(&mut settings, at_exit), "saved on exit");
    assert_eq!(settings.main_window_size, Some([a.x, a.y]));
    // Settled and queued, but the app quit before it took it.
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    r.shared.lock().unwrap().size_to_save = Some([a.x, a.y]);
    assert_eq!(r.shared.lock().unwrap().size_at_exit(), Some([a.x, a.y]));
    assert_eq!(r.shared.lock().unwrap().size_at_exit(), None, "taken once");
    // Nothing made by hand: nothing to save, and a size already saved is not saved again.
    let mut r = Run::new(work_4k_at_150, Some([a.x, a.y]));
    r.app_frame(Some(Request::Open));
    r.normal_frames(60, a);
    assert_eq!(r.shared.lock().unwrap().size_at_exit(), None);
    let mut settings = r.shared.lock().unwrap().prefs.settings.clone();
    assert!(!size_changes(&mut settings, None));
    assert!(!size_changes(&mut settings, Some([a.x, a.y])), "the same size: no write");
    // Closed and quit: the close took it already.
    let mut r = Run::new(work_4k_at_150, None);
    r.app_frame(Some(Request::Open));
    r.normal_frames(5, a);
    r.close();
    assert_eq!((take_saves(), r.shared.lock().unwrap().size_at_exit()), (vec![[a.x, a.y]], None));
}
