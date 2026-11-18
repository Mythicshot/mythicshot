//! Mythicshot: a tray app that captures the screen.
//!
//! The root eframe window is the region-selector overlay. It stays hidden
//! until a capture starts, then covers the whole desktop. Hotkeys, tray menu
//! clicks and raw key events (only while the Preferences page records a new
//! shortcut) arrive on one channel and wake the UI. The main window (Home,
//! Tours, Workspace, Preferences) is a second window, see `main_window`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod editor_ui;
mod home_ui;
mod hotkeys;
mod hook;
mod icons;
mod input;
mod live_draw;
mod main_window;
mod overlay;
mod password;
mod platform;
mod popups;
mod project;
mod publish;
mod publish_ui;
mod recorder_ui;
mod settings;
mod settings_ui;
mod site;
mod site_ui;
mod tray;
mod tour;
mod tour_cards;
mod tour_view;
mod tours_ui;
mod video;
mod video_editor;
mod workspace_ui;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, ViewportBuilder, ViewportCommand, pos2, vec2};
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use mythicshot_capture::{PxRect, grab_desktop};
use mythicshot_destinations::{self as dest, DestId, Sent};
use tray_icon::TrayIconEvent;
use tray_icon::menu::MenuEvent;

use hotkeys::{Action, EditorAction, Hotkeys, Recorded, ShortcutRecorder};
use overlay::{Outcome, Overlay};
use settings::Settings;
use settings_ui::{Bind, SettingsState, UiAction};
use tray::{Command, Tray};

enum AppEvent {
    Hotkey(u32),
    /// A Print Screen shortcut taken by the input hook.
    Taken(Action),
    Menu(tray_icon::menu::MenuId),
    TrayClick,
    Key(rdev::EventType),
    /// A key or click logged for the recording in progress.
    Input(mythicshot_editor::overlays::InputEvent),
}

struct App {
    settings: Settings,
    hotkeys: Hotkeys,
    tray: Tray,
    rx: Receiver<AppEvent>,
    tx: Sender<AppEvent>,
    overlay: Option<Overlay>,
    /// The main window's state, shared with its viewport.
    window: Arc<std::sync::Mutex<main_window::Shared>>,
    /// The page the main window was asked to open on, until its next frame.
    window_request: Option<main_window::Request>,
    /// The window the user was in before Mythicshot, to give focus back to before a capture.
    last_foreground: main_window::LastForeground,
    /// A capture started from the main window, waiting for the window to be gone.
    hide_then_run: main_window::HideThenRun,
    settings_state: SettingsState,
    recorder: ShortcutRecorder,
    key_hook_started: bool,
    /// Open editors; the flag is true until the window has been shown once.
    editors: Vec<(editor_ui::EditorWindow, bool)>,
    icon: Arc<egui::IconData>,
    listening: Arc<AtomicBool>,
    snipping_tool: Option<bool>,
    pickers: Vec<popups::PickerWindow>,
    toast: popups::Toast,
    /// The recording in progress, and GIFs being encoded.
    session: Option<recorder_ui::Session>,
    /// Key and click events for the recording in progress.
    rec_events: Vec<mythicshot_editor::overlays::InputEvent>,
    /// Read by the input hook thread: where to send events while recording.
    tap: Arc<std::sync::Mutex<Option<input::Tap>>>,
    /// When to save editor preferences that changed.
    prefs_save_at: Option<Instant>,
    /// A save of the settings file failed: what is to be saved is still in memory, and is saved again later.
    save_retry: settings::SaveRetry,
    encoding: Vec<recorder_ui::EncodeJob>,
    /// The root window is kept out of screen capture once its handle is known.
    root_hidden_from_capture: bool,
    /// When to give focus back to the app being recorded.
    refocus_at: Option<Instant>,
    hidden_at_start: bool,
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    started: Instant,
    /// Wakes the UI from the input hook's thread. The hook must answer
    /// Windows quickly or Windows quietly removes it, and asking egui for a
    /// repaint can wait on the UI thread, so a relay thread does that instead.
    wake: mpsc::Sender<()>,
    /// Draw was pressed during the countdown.
    draw_when_live: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let (tx, rx) = mpsc::channel();

        let hk_tx = tx.clone();
        let hk_ctx = ctx.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.state() == HotKeyState::Pressed {
                let _ = hk_tx.send(AppEvent::Hotkey(e.id()));
                hk_ctx.request_repaint();
            }
        }));
        let menu_tx = tx.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = menu_tx.send(AppEvent::Menu(e.id));
            menu_ctx.request_repaint();
        }));
        let tray_tx = tx.clone();
        let tray_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: tray_icon::MouseButton::Left, button_state: tray_icon::MouseButtonState::Up, .. } = e {
                let _ = tray_tx.send(AppEvent::TrayClick);
                tray_ctx.request_repaint();
            }
        }));

        let mut settings = Settings::load();
        // Once per install, and on disk before anything else can change the address.
        let mut save_retry = settings::SaveRetry::default();
        if publish_ui::settle_first_binding(&mut settings) {
            save_retry.record(&settings.save(), Instant::now());
        }
        publish_ui::init(&settings);
        let mut hotkeys = Hotkeys::new();
        hotkeys.take_print_screen = settings.take_print_screen;
        for a in Action::ALL {
            hotkeys.apply(*a, settings.binding(*a));
            match (hotkeys.errors.get(a), settings.binding(*a)) {
                (Some(e), b) => eprintln!("hotkey {} [{b}]: {e}", a.label()),
                (None, "") => eprintln!("hotkey {}: no shortcut", a.label()),
                (None, b) if hotkeys.is_taken(*a) => eprintln!("hotkey {} [{b}]: taken by the input hook", a.label()),
                (None, b) => eprintln!("hotkey {} [{b}]: registered", a.label()),
            }
        }
        let displays: Vec<String> = mythicshot_capture::list_monitors()
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let primary = if m.primary { " (primary)" } else { "" };
                format!("Display {}{primary}  ·  {}×{}", i + 1, m.rect.w, m.rect.h)
            })
            .collect();
        let tray = Tray::new(&settings, &displays).expect("create tray icon");
        let window = Arc::new(std::sync::Mutex::new(main_window::Shared::new(settings_ui::Shared {
            settings: settings.clone(),
            errors: HashMap::new(),
            snipping_tool: None,
            autostart: false,
            state: SettingsState::default(),
            actions: Vec::new(),
        })));
        // Deletes a past run could not finish: the Tours page offers to carry them on.
        if let Ok(s) = window.lock() {
            // A delete is noted in the file before its first request, on the job's own thread.
            // A file that could not be read at start-up is never written over, here either:
            // with no way to write its note, a new delete is refused on the Tours page.
            s.tours.unfinished().start(settings.unfinished_deletes.clone(), settings.note_saver());
        }

        let mut app = Self {
            settings,
            hotkeys,
            tray,
            rx,
            tx,
            overlay: None,
            window,
            window_request: main_window::start_request(std::env::args()),
            last_foreground: main_window::LastForeground::default(),
            hide_then_run: main_window::HideThenRun::default(),
            settings_state: SettingsState::default(),
            recorder: ShortcutRecorder::default(),
            key_hook_started: false,
            editors: Vec::new(),
            icon: Arc::new(egui::IconData { rgba: tray::icon_rgba(64).0, width: 64, height: 64 }),
            listening: Arc::new(AtomicBool::new(false)),
            snipping_tool: platform::snipping_tool_owns_prtsc(),
            pickers: Vec::new(),
            toast: popups::Toast::new(),
            session: None,
            rec_events: Vec::new(),
            tap: Arc::new(std::sync::Mutex::new(None)),
            prefs_save_at: None,
            save_retry,
            encoding: Vec::new(),
            root_hidden_from_capture: false,
            refocus_at: None,
            hidden_at_start: false,
            started: Instant::now(),
            draw_when_live: false,
            wake: {
                let (wake, woken) = mpsc::channel::<()>();
                let relay = ctx.clone();
                std::thread::spawn(move || {
                    while woken.recv().is_ok() {
                        relay.request_repaint();
                    }
                });
                wake
            },
        };
        platform::stop_raw_keyboard_input();
        app.publish_taken_keys();
        app
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AppEvent::Hotkey(_) | AppEvent::Taken(_) => {
                    if self.listening.load(Ordering::Relaxed) || self.overlay.is_some() {
                        continue;
                    }
                    let action = match ev {
                        AppEvent::Taken(a) => a,
                        AppEvent::Hotkey(id) => match self.hotkeys.action_for(id) {
                            Some(a) => a,
                            None => continue,
                        },
                        _ => continue,
                    };
                    if self.session.is_some() {
                        // While recording, the record keys stop it and the pause key pauses.
                        match action {
                            Action::PauseRecording => self.recording_action(ctx, recorder_ui::BarAction::PauseToggle),
                            Action::LiveDraw => self.recording_action(ctx, recorder_ui::BarAction::ToggleDraw),
                            Action::RecordRegion | Action::RecordLastRegion | Action::RecordWindow | Action::RecordFullScreen => {
                                self.recording_action(ctx, recorder_ui::BarAction::Stop)
                            }
                            _ => {}
                        }
                        continue;
                    }
                    self.run(ctx, Command::from_action(action));
                }
                AppEvent::Menu(id) => {
                    if let Some(cmd) = self.tray.command(&id) {
                        self.run(ctx, cmd);
                    }
                }
                AppEvent::TrayClick => {
                    if let Some(cmd) = tray::left_click(self.settings.tray_left_click, self.activity()) {
                        self.run(ctx, cmd);
                    }
                }
                AppEvent::Key(ev) => self.record_key(&ev),
                AppEvent::Input(ev) => {
                    if self.session.is_some() {
                        self.rec_events.push(ev);
                    }
                }
            }
        }
    }

    /// What is on screen that a tray click must not disturb. A capture started from the main
    /// window counts from the moment its tile is clicked.
    fn activity(&self) -> tray::Activity {
        tray::Activity { overlay: self.overlay.is_some(), session: self.session.is_some(), picker: !self.pickers.is_empty(), pending: self.hide_then_run.is_waiting() }
    }

    fn run(&mut self, ctx: &egui::Context, cmd: Command) {
        // The main window never opens over a capture, whether asked for by a click or from the menu.
        if !tray::allowed(cmd, self.activity()) {
            return;
        }
        // A capture waiting for the main window to be gone gives way to whatever runs now.
        if cmd.changes_the_screen() {
            self.hide_then_run.cancel();
        }
        match cmd {
            Command::Region => self.start_overlay(ctx),
            Command::LastRegion => match self.settings.last_region {
                Some([x, y, w, h]) => self.capture_now(ctx, |_| Some((PxRect::new(x, y, w as u32, h as u32), "Region".into()))),
                None => self.start_overlay(ctx),
            },
            Command::Window => self.capture_now(ctx, |d| d.focused_window().map(|w| (w.rect, w.title.clone()))),
            Command::FullScreen => self.capture_now(ctx, |d| Some((d.bounds, "Full screen".into()))),
            Command::Record => self.start_overlay_mode(ctx, true),
            Command::RecordLastRegion => match self.settings.last_region {
                Some([x, y, w, h]) => self.start_recording(ctx, PxRect::new(x, y, w as u32, h as u32), "Region".into()),
                None => self.start_overlay_mode(ctx, true),
            },
            Command::RecordWindow => match grab_desktop().ok().and_then(|d| d.focused_window().map(|w| (w.rect, w.title.clone()))) {
                Some((rect, title)) => self.start_recording(ctx, rect, title),
                None => self.notify("No window to record".into()),
            },
            Command::RecordFullScreen => {
                let (cx, cy) = platform::cursor_pos().unwrap_or((0, 0));
                let monitors = mythicshot_capture::list_monitors();
                match monitors.iter().find(|m| m.rect.contains(cx, cy)).or_else(|| monitors.iter().find(|m| m.primary)) {
                    Some(m) => self.start_recording(ctx, m.rect, "Full screen".into()),
                    None => self.notify("No screen to record".into()),
                }
            }
            Command::PauseRecording => self.recording_action(ctx, recorder_ui::BarAction::PauseToggle),
            Command::LiveDraw => self.recording_action(ctx, recorder_ui::BarAction::ToggleDraw),
            Command::Display(i) => self.capture_now(ctx, move |d| d.monitors.get(i).map(|m| (m.rect, format!("Display {}", i + 1)))),
            Command::FromClipboard => match dest::image_from_clipboard() {
                Ok(img) => self.captured(ctx, img, "Clipboard image".into()),
                Err(e) => self.notify(e),
            },
            Command::OpenFile => self.open_file(),
            Command::ToggleDestination(d) => {
                self.settings.toggle_destination(d);
                self.settings_changed();
            }
            Command::ToggleMagnifier => {
                self.settings.magnifier = !self.settings.magnifier;
                self.settings_changed();
            }
            // The main window, on Home or on Preferences. Already open, it comes to the front on that page.
            Command::OpenApp | Command::Preferences => self.window_request = main_window::request_for(cmd),
            Command::OpenFolder => platform::open_folder(&self.settings.output_dir),
            Command::Exit => ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }

    fn start_overlay(&mut self, ctx: &egui::Context) {
        self.start_overlay_mode(ctx, false);
    }

    fn start_overlay_mode(&mut self, ctx: &egui::Context, record: bool) {
        if self.session.is_some() {
            return;
        }
        let desktop = match grab_desktop() {
            Ok(d) => d,
            Err(e) => return self.notify(format!("Capture failed: {e}")),
        };
        let ppp = ctx.pixels_per_point();
        let b = desktop.bounds;
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(b.x as f32 / ppp, b.y as f32 / ppp)));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(b.w as f32 / ppp, b.h as f32 / ppp)));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.overlay = Some(Overlay::new(desktop, self.settings.magnifier, record));
    }

    /// Freeze the desktop and take a rectangle (and a title for it) without showing the overlay.
    fn capture_now(&mut self, ctx: &egui::Context, pick: impl FnOnce(&mythicshot_capture::Desktop) -> Option<(PxRect, String)>) {
        let desktop = match grab_desktop() {
            Ok(d) => d,
            Err(e) => return self.notify(format!("Capture failed: {e}")),
        };
        match pick(&desktop).and_then(|(r, title)| desktop.crop(r).map(|img| (img, title))) {
            Some((img, title)) => self.captured(ctx, img, title),
            None => self.notify("Nothing to capture there".into()),
        }
    }

    fn finish_overlay(&mut self, ctx: &egui::Context, outcome: Outcome) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        let Some(ov) = self.overlay.take() else { return };
        self.settings.magnifier = ov.magnifier;
        if let Outcome::Capture(rect) = outcome {
            self.settings.last_region = Some([rect.x, rect.y, rect.w as i32, rect.h as i32]);
            self.save_settings();
            // A window picked in the overlay keeps its title for the file name.
            let title = ov.desktop.windows.iter().find(|w| w.rect == rect).map(|w| w.title.clone()).unwrap_or_else(|| "Region".into());
            if ov.record {
                self.start_recording(ctx, rect, title);
            } else if let Some(img) = ov.desktop.crop(rect) {
                self.captured(ctx, img, title);
            }
        }
    }

    /// Show the red frame and control bar, count down, then record `rect`.
    fn start_recording(&mut self, ctx: &egui::Context, rect: PxRect, title: String) {
        if self.session.is_some() {
            return;
        }
        match recorder_ui::Session::new(rect, title, self.settings.countdown_secs, self.settings.record_fps) {
            Ok(s) => {
                self.rec_events.clear();
                let ppp = ctx.pixels_per_point();
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(s.bar_position(ppp)));
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(recorder_ui::BAR_SIZE[0], recorder_ui::BAR_SIZE[1])));
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                self.refocus_at = Some(Instant::now() + Duration::from_millis(150));
                self.session = Some(s);
                self.draw_when_live = false;
                self.hotkeys.set_recording(true);
                self.publish_taken_keys();
            }
            Err(e) => self.notify(e),
        }
    }

    /// A control-bar button or recording hotkey.
    fn recording_action(&mut self, ctx: &egui::Context, action: recorder_ui::BarAction) {
        use recorder_ui::BarAction;
        match action {
            BarAction::ToggleKeys => {
                self.settings.record_keys = !self.settings.record_keys;
                self.settings_changed();
                if let Ok(mut t) = self.tap.lock() {
                    if let Some(t) = t.as_mut() {
                        t.set_keys(self.settings.record_keys);
                    }
                }
            }
            BarAction::PauseToggle => {
                if let Some(s) = self.session.as_mut() {
                    s.toggle_pause();
                }
            }
            BarAction::ToggleDraw => {
                let Some(s) = self.session.as_mut() else { return };
                let Some(clock) = s.clock() else {
                    // Still counting down: start drawing when recording starts.
                    self.draw_when_live = !self.draw_when_live;
                    return;
                };
                let on = !s.draw.is_on();
                s.draw.set_on(on, Some(clock));
                if let Ok(mut t) = self.tap.lock() {
                    if let Some(t) = t.as_mut() {
                        t.drawing = on;
                    }
                }
                if !on {
                    // Give focus back to the app being recorded.
                    self.refocus_at = Some(Instant::now() + Duration::from_millis(150));
                }
            }
            BarAction::Restart => {
                if let Some(Err(e)) = self.session.as_mut().map(|s| s.restart()) {
                    self.end_session(ctx);
                    self.notify(e);
                }
            }
            BarAction::Discard => {
                if let Some(s) = self.session.take() {
                    s.discard();
                }
                self.end_session(ctx);
            }
            BarAction::Stop => {
                let Some(mut s) = self.session.take() else { return };
                let title = s.title.clone();
                let draw = std::mem::take(&mut s.draw);
                self.end_session(ctx);
                match s.stop() {
                    None => {}
                    Some(Err(e)) => self.notify(e),
                    Some(Ok(rec)) if rec.frames.is_empty() => {
                        rec.delete();
                        self.notify("Nothing was recorded".into());
                    }
                    Some(Ok(rec)) => {
                        let events = std::mem::take(&mut self.rec_events);
                        let items = draw.into_items(&rec);
                        self.recording_done(rec, events, items, title);
                    }
                }
            }
        }
    }

    /// Hide the control bar and stop logging input; the frame window goes when
    /// it is no longer shown.
    fn end_session(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.refocus_at = None;
        if let Ok(mut t) = self.tap.lock() {
            *t = None;
        }
        // The recording keys go back to other apps.
        self.hotkeys.set_recording(false);
        self.publish_taken_keys();
    }

    /// A finished recording: open it in the GIF editor if the editor is a
    /// destination, otherwise export a GIF straight away.
    fn recording_done(&mut self, rec: mythicshot_recording::Recording, events: Vec<mythicshot_editor::overlays::InputEvent>, items: Vec<mythicshot_editor::Item>, title: String) {
        let keys = events.iter().filter(|e| matches!(e.kind, mythicshot_editor::overlays::InputKind::KeyDown(_))).count();
        eprintln!("recording: {} frames, {} input events ({keys} key presses)", rec.frames.len(), events.len());
        let rec = Arc::new(rec);
        if self.settings.has_destination(DestId::Editor) || self.settings.has_destination(DestId::Picker) {
            self.open_video_editor(rec, events, items, title);
            return;
        }
        match dest::new_path_with_extension(&self.settings.dest_context(&title), "gif") {
            Ok(path) => {
                self.toast.post("Encoding GIF…".into(), None);
                let edit = video::Edit::new(&rec, self.settings.overlays);
                let spec = recorder_ui::ExportSpec { format: mythicshot_recording::export::Format::Gif, quality: self.settings.gif_quality, loop_forever: true, video_fps: self.settings.record_fps };
                self.encoding.push(recorder_ui::EncodeJob::start(rec, items, events, edit, spec, path, true));
            }
            Err(e) => {
                rec.delete_files();
                self.notify(e);
            }
        }
    }

    /// Each frame while recording: countdown, frame window, errors, focus.
    fn tick_recording(&mut self, ctx: &egui::Context) {
        // Once recording is live, point the input hook at it.
        if let Some(clock) = self.session.as_ref().and_then(|s| s.clock()) {
            let origin = self.session.as_ref().map(|s| (s.rect.x, s.rect.y)).unwrap_or_default();
            let mut started = false;
            if let Ok(mut t) = self.tap.lock() {
                if t.is_none() {
                    *t = Some(input::Tap::new(clock, origin, self.settings.record_keys));
                    started = true;
                }
            }
            if started {
                self.ensure_input_hook();
                if std::mem::take(&mut self.draw_when_live) {
                    self.recording_action(ctx, recorder_ui::BarAction::ToggleDraw);
                }
            }
        }
        if let Some(s) = self.session.as_mut() {
            if let Err(e) = s.tick() {
                self.recording_action(ctx, recorder_ui::BarAction::Stop);
                self.notify(e);
                return;
            }
            s.show_frame(ctx);
            let rect = s.rect;
            s.draw.show(ctx, rect);
        }
        if self.session.as_ref().is_some_and(|s| s.draw.take_done()) {
            self.recording_action(ctx, recorder_ui::BarAction::ToggleDraw);
        }
        if self.refocus_at.is_some_and(|t| Instant::now() >= t) {
            self.refocus_at = None;
            if let Some(h) = self.session.as_ref().and_then(|s| s.previous_foreground) {
                platform::set_foreground(h);
            }
        }
        // GIFs being encoded: progress in the toast, then the result.
        let mut finished = Vec::new();
        for (i, job) in self.encoding.iter().enumerate() {
            match job.take_result() {
                Some(r) => finished.push((i, r)),
                None => self.toast.post(format!("Encoding GIF… {:.0}%", job.progress() * 100.0), None),
            }
        }
        for (i, r) in finished.into_iter().rev() {
            self.encoding.remove(i);
            match r {
                Ok((path, size)) => {
                    let msg = format!("Saved GIF ({:.1} MB) {}", size as f64 / 1_048_576.0, path.display());
                    eprintln!("{msg}");
                    self.toast.post(msg, Some(path));
                }
                Err(e) => self.notify(format!("GIF export failed: {e}")),
            }
        }
    }

    /// A new capture: show the picker, or send it to every chosen destination.
    fn captured(&mut self, ctx: &egui::Context, img: image::RgbaImage, title: String) {
        if self.settings.has_destination(DestId::Picker) {
            self.pickers.push(popups::PickerWindow::new(img, title, ctx.pixels_per_point()));
        } else {
            let list: Vec<DestId> = self.settings.destinations().to_vec();
            self.send_to(img, title, &list);
        }
    }

    /// Run destinations in order and confirm with one toast.
    fn send_to(&mut self, img: image::RgbaImage, title: String, list: &[DestId]) {
        let list: Vec<DestId> = if list.is_empty() { vec![DestId::Editor] } else { list.to_vec() };
        let mut messages = Vec::new();
        let mut path = None;
        for d in list {
            match d {
                DestId::Editor => self.open_editor(img.clone(), title.clone()),
                DestId::Picker => {}
                d => match dest::send(d, &img, &self.settings.dest_context(&title)) {
                    Ok(Sent::Cancelled) => {}
                    Ok(sent) => {
                        if path.is_none() {
                            path = sent.path().map(|p| p.to_path_buf());
                        }
                        messages.push(sent.describe());
                    }
                    Err(e) => messages.push(e),
                },
            }
        }
        if !messages.is_empty() {
            eprintln!("{}", messages.join(" | "));
            self.toast.post(messages.join("\n"), path);
        }
    }

    fn show_pickers(&mut self, ctx: &egui::Context) {
        let mut chosen = Vec::new();
        self.pickers.retain_mut(|p| {
            let Ok(mut state) = p.state.lock() else { return false };
            match std::mem::replace(&mut state.pick, popups::Pick::Waiting) {
                popups::Pick::Waiting => true,
                popups::Pick::Cancelled => false,
                popups::Pick::Chosen(d) => {
                    let img = std::mem::replace(&mut state.img, image::RgbaImage::new(1, 1));
                    chosen.push((img, state.title.clone(), d));
                    false
                }
            }
        });
        for (img, title, d) in chosen {
            self.send_to(img, title, &[d]);
        }
        for p in &mut self.pickers {
            p.show(ctx);
        }
    }

    fn open_editor(&mut self, img: image::RgbaImage, title: String) {
        let editor = editor_ui::Editor::new(img, title.clone(), self.settings.editor_keys.clone(), self.settings.dest_context(&title), self.settings.editor.clone());
        self.editors.push((editor_ui::EditorWindow::new(editor), true));
    }

    /// Open a recording in the editor's video mode (the GIF editor).
    fn open_video_editor(&mut self, rec: Arc<mythicshot_recording::Recording>, events: Vec<mythicshot_editor::overlays::InputEvent>, items: Vec<mythicshot_editor::Item>, title: String) {
        let s = &self.settings;
        match editor_ui::Editor::new_video(rec.clone(), events, items, title.clone(), s.editor_keys.clone(), s.dest_context(&title), s.overlays, s.editor.clone()) {
            Ok(editor) => self.editors.push((editor_ui::EditorWindow::new(editor), true)),
            Err(e) => {
                rec.delete_files();
                self.notify(format!("Could not open the recording: {e}"));
            }
        }
    }

    fn open_file(&mut self) {
        let picked = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "bmp", "gif", "webp"]).set_directory(&self.settings.output_dir).pick_file();
        let Some(path) = picked else { return };
        match image::open(&path) {
            Ok(img) => {
                let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.open_editor(img.to_rgba8(), title);
            }
            Err(e) => self.notify(format!("Could not open {}: {e}", path.display())),
        }
    }

    fn show_editors(&mut self, ctx: &egui::Context) {
        self.collect_editor_prefs();
        self.editors.retain(|(w, _)| !w.is_closed());
        for (w, first) in &mut self.editors {
            w.show(ctx, self.icon.clone(), std::mem::take(first));
        }
    }

    /// Pick up styles, tool and export choices changed in any editor, share
    /// them with the other open editors, and save them shortly after the last
    /// change (a slider drag changes them every frame).
    fn collect_editor_prefs(&mut self) {
        let mut changed: Option<settings::EditorPrefs> = None;
        for (w, _) in &self.editors {
            if let Ok(mut e) = w.editor.lock() {
                if std::mem::take(&mut e.prefs_changed) {
                    changed = Some(e.prefs.clone());
                }
                if let Some(o) = e.overlay_default.take() {
                    self.settings.overlays = o;
                    self.prefs_save_at = Some(Instant::now() + Duration::from_millis(500));
                }
            }
        }
        if let Some(prefs) = changed {
            for (w, _) in &self.editors {
                if let Ok(mut e) = w.editor.lock() {
                    // Other editors keep their current tool; they get the styles and export choices.
                    let tool = std::mem::take(&mut e.prefs.tool);
                    e.prefs = prefs.clone();
                    e.prefs.tool = tool;
                }
            }
            self.settings.editor = prefs;
            self.prefs_save_at = Some(Instant::now() + Duration::from_millis(500));
        }
        if self.prefs_save_at.is_some_and(|t| Instant::now() >= t) {
            self.prefs_save_at = None;
            self.save_settings();
        }
    }

    fn notify(&mut self, msg: String) {
        eprintln!("{msg}");
        self.toast.post(msg, None);
    }

    /// Write to the settings file what the tour pages and their background jobs noted since the
    /// last look: a delete that did not finish (or one that now has), and the address the saved
    /// token belongs to. Neither is a secret, and neither is edited on the Preferences page.
    ///
    /// A save that failed earlier is tried again here too, a while after it failed; `now_or_never`
    /// (the app is closing) does not wait for that.
    fn save_publish_state(&mut self, now_or_never: bool) {
        let unfinished = self.window.lock().is_ok_and(|s| s.tours.unfinished().is_changed());
        let wanted = unfinished || publish_ui::has_token_site_change() || self.save_retry.pending();
        if wanted && (now_or_never || self.save_retry.allows(Instant::now())) {
            self.save_settings();
        }
    }

    /// Every save of the settings file goes through here: what the background jobs noted is
    /// taken in first, with their list held still, so a save can never write an older list
    /// over a note a job has just put on disk.
    fn save_settings(&mut self) {
        if let Some(site) = publish_ui::take_token_site_change() {
            self.settings.publish_saved_for = Some(site);
        }
        let unfinished = self.window.lock().map(|s| s.tours.unfinished()).ok();
        let mut result = Ok(());
        match unfinished {
            Some(unfinished) => {
                unfinished.settle(|list| {
                    if let Some(list) = list {
                        self.settings.unfinished_deletes = list;
                    }
                    result = self.settings.save();
                    result.is_ok()
                });
            }
            None => result = self.settings.save(),
        }
        // A save that failed loses nothing: the settings stay in memory, and are saved again
        // by the next change, a while from now, or when the app closes.
        self.save_retry.record(&result, Instant::now());
    }

    fn settings_changed(&mut self) {
        self.save_settings();
        publish_ui::set_server(&self.settings.publish_server);
        self.tray.sync(&self.settings);
        for (w, _) in &self.editors {
            if let Ok(mut e) = w.editor.lock() {
                e.keys = self.settings.editor_keys.clone();
                e.set_output(self.settings.dest_context(""));
            }
        }
    }

    /// Register every global shortcut again, after the Print Screen choice changed.
    fn apply_hotkeys(&mut self) {
        self.hotkeys.take_print_screen = self.settings.take_print_screen;
        for a in Action::ALL {
            let b = self.settings.binding(*a).to_string();
            self.hotkeys.apply(*a, &b);
        }
        self.publish_taken_keys();
    }

    /// Tell the input hook which shortcuts to take now (recording ones only
    /// while a recording is on), starting it if any are.
    fn publish_taken_keys(&mut self) {
        let taken = self.hotkeys.taken();
        if taken.is_empty() && !self.key_hook_started {
            return;
        }
        let (tx, wake, listening) = (self.tx.clone(), self.wake.clone(), self.listening.clone());
        hook::set_taken_keys(taken, move |a| {
            // Recording a new shortcut: let the key through to the recorder.
            if listening.load(Ordering::Relaxed) {
                return false;
            }
            let _ = tx.send(AppEvent::Taken(a));
            let _ = wake.send(());
            true
        });
        self.ensure_input_hook();
    }

    fn set_binding(&mut self, bind: Bind, binding: String) {
        match bind {
            Bind::Global(action) => {
                self.settings.hotkeys.insert(action, binding.clone());
                self.hotkeys.apply(action, &binding);
                if hotkeys::print_screen_mods(&binding).is_some() || action.while_recording() || self.key_hook_started {
                    self.publish_taken_keys();
                }
            }
            Bind::Editor(action) => {
                self.settings.editor_keys.insert(action, binding);
            }
        }
        self.settings_changed();
    }

    /// Errors for the Hotkeys tab: global shortcuts the OS refused, and editor
    /// shortcuts used twice (the second one would never fire).
    fn binding_errors(&self) -> HashMap<Bind, String> {
        let mut errors: HashMap<Bind, String> = self.hotkeys.errors.iter().map(|(a, e)| (Bind::Global(*a), e.clone())).collect();
        let mut seen: HashMap<String, EditorAction> = HashMap::new();
        for a in EditorAction::ALL {
            let b = self.settings.editor_binding(*a).to_ascii_lowercase();
            if b.is_empty() {
                continue;
            }
            match seen.get(&b) {
                Some(first) => {
                    errors.insert(Bind::Editor(*a), format!("Already used by \"{}\"", first.label()));
                }
                None => {
                    seen.insert(b, *a);
                }
            }
        }
        errors
    }

    fn listen(&mut self, action: Option<Bind>) {
        self.settings_state.listening = action;
        self.recorder = ShortcutRecorder::default();
        self.listening.store(action.is_some(), Ordering::Relaxed);
        if action.is_some() {
            self.ensure_input_hook();
        }
    }

    /// Start the one input hook this process may have (rdev allows one). It
    /// forwards keys only while the settings page records a shortcut, and keys
    /// and clicks only while a recording is running.
    fn ensure_input_hook(&mut self) {
        if self.key_hook_started {
            return;
        }
        self.key_hook_started = true;
        let (tx, flag, tap, wake) = (self.tx.clone(), self.listening.clone(), self.tap.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let mut pointer = (0.0, 0.0);
            let result = hook::listen(move |ev: rdev::EventType| {
                if let rdev::EventType::MouseMove { x, y } = ev {
                    pointer = (x, y);
                    return;
                }
                if flag.load(Ordering::Relaxed) {
                    let _ = tx.send(AppEvent::Key(ev));
                    let _ = wake.send(());
                }
                // try_lock: the hook must never wait (see `wake`).
                if let Some(ev) = tap.try_lock().ok().and_then(|mut t| t.as_mut().and_then(|t| t.convert(&ev, pointer))) {
                    let _ = tx.send(AppEvent::Input(ev));
                }
            });
            if let Err(e) = result {
                eprintln!("input hook unavailable: {e}");
            }
        });
    }

    fn record_key(&mut self, ev: &rdev::EventType) {
        let Some(action) = self.settings_state.listening else { return };
        match self.recorder.feed(ev) {
            Recorded::Pending => {}
            Recorded::Cancel => self.stop_listening(),
            Recorded::Clear => {
                self.stop_listening();
                self.set_binding(action, String::new());
            }
            Recorded::Done(binding) => {
                self.stop_listening();
                self.set_binding(action, binding);
            }
        }
    }

    fn stop_listening(&mut self) {
        self.settings_state.listening = None;
        self.listening.store(false, Ordering::Relaxed);
    }

    /// Each frame: apply what the main window queued, start a capture it asked for once it is
    /// hidden, and keep it drawn while it is open.
    fn main_window(&mut self, ctx: &egui::Context) {
        self.save_publish_state(false);
        if self.save_retry.pending() {
            ctx.request_repaint_after(settings::SAVE_RETRY_AFTER);
        }
        // Pull what the window did since our last frame.
        let (actions, edited, tab, capture) = {
            let mut s = self.window.lock().expect("main window state");
            s.settle(ctx);
            let actions = std::mem::take(&mut s.prefs.actions);
            let edited = if actions.is_empty() { None } else { Some(s.prefs.settings.clone()) };
            (actions, edited, s.prefs.state.tab, s.capture.take())
        };
        self.settings_state.tab = tab;
        let edited = edited.unwrap_or_else(|| self.settings.clone());
        // Which app the user is in, for a capture started from the main window.
        if let Some(front) = platform::foreground_window() {
            self.last_foreground.note(front, platform::window_kind(front));
        }
        for a in actions {
            match a {
                UiAction::Listen(action) => self.listen(Some(action)),
                UiAction::StopListening => self.stop_listening(),
                UiAction::SetBinding(action, b) => {
                    self.stop_listening();
                    self.set_binding(action, b);
                }
                UiAction::ResetAll => {
                    self.stop_listening();
                    for a in Action::ALL {
                        self.set_binding(Bind::Global(*a), a.default_binding().to_string());
                    }
                    for a in EditorAction::ALL {
                        self.set_binding(Bind::Editor(*a), a.default_binding().to_string());
                    }
                }
                UiAction::OpenSettingsFolder => {
                    if let Some(dir) = Settings::path().parent() {
                        platform::open_folder(dir);
                    }
                }
                UiAction::SettingsChanged => {
                    // The General and Output tabs and the Workspace page edit fields directly; shortcuts go through SetBinding.
                    self.settings.output_dir = edited.output_dir.clone();
                    self.settings.destinations = edited.destinations.clone();
                    self.settings.filename_pattern = edited.filename_pattern.clone();
                    self.settings.image_format = edited.image_format;
                    self.settings.jpeg_quality = edited.jpeg_quality;
                    self.settings.magnifier = edited.magnifier;
                    self.settings.record_fps = edited.record_fps;
                    self.settings.countdown_secs = edited.countdown_secs;
                    self.settings.record_keys = edited.record_keys;
                    self.settings.overlays = edited.overlays;
                    self.settings.gif_quality = edited.gif_quality;
                    self.settings.publish_server = edited.publish_server.clone();
                    self.settings.tray_left_click = edited.tray_left_click;
                    self.settings_changed();
                }
                UiAction::SetTakePrintScreen(on) => {
                    self.settings.take_print_screen = on;
                    self.apply_hotkeys();
                    self.settings_changed();
                }
                UiAction::SetAutostart(on) => {
                    if let Err(e) = platform::set_autostart(on) {
                        self.notify(e);
                    }
                }
                // A page switch: the main window takes it and never queues it.
                UiAction::OpenWorkspace => {}
            }
        }
        // A Home tile: close the window so it is not in the picture, then capture. It stays
        // closed afterwards, so it never comes back over the overlay or the editor.
        if let Some(cmd) = capture {
            if let Ok(mut s) = self.window.lock() {
                s.nav.close();
                s.settle(ctx);
            }
            self.window_request = None;
            // Focus goes back to the app the user came from, so "Window" captures that one.
            if let Some(previous) = self.last_foreground.get() {
                platform::set_foreground(previous);
            }
            self.hide_then_run.start(cmd, Instant::now());
        }
        if let Some(cmd) = self.hide_then_run.take_due(Instant::now()) {
            // Anything started in the meantime dropped it already (see `run`); this is the last check.
            if tray::pending_may_run(self.activity()) {
                // "Window" never captures one of our own windows.
                let front = platform::foreground_window().map(platform::window_kind);
                self.run(ctx, main_window::tile_command(cmd, front));
            }
        }
        if let Some(wait) = self.hide_then_run.due_in(Instant::now()) {
            ctx.request_repaint_after(wait);
        }

        // Push the current state back and keep the window drawn while it is open.
        // A closed window costs nothing more per frame.
        let open = self.window.lock().map(|s| s.nav.is_open()).unwrap_or(false);
        if !open && self.window_request.is_none() {
            return;
        }
        {
            let mut s = self.window.lock().expect("main window state");
            s.busy = self.activity().busy();
            let p = &mut s.prefs;
            p.settings = self.settings.clone();
            p.errors = self.binding_errors();
            p.snipping_tool = self.snipping_tool;
            p.autostart = platform::autostart_enabled();
            p.state.listening = self.settings_state.listening;
        }
        main_window::show(ctx, &self.window, self.icon.clone(), self.window_request.take());
    }
}

impl eframe::App for App {
    /// A publish or tour export still running is cancelled and given a few seconds to remove what it
    /// wrote (a tour export's temporary file or partial folder); a publish's temporary folder goes if it
    /// has not ended in time.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        publish_ui::shutdown(std::time::Duration::from_secs(5));
        // A delete that was noted as unfinished in the last moments is on disk before the app is gone,
        // and so is a change whose save failed earlier, if the file can be written by now.
        self.save_publish_state(true);
        // An add to a site was cancelled above; one already swapping its tour in finishes or rolls back first.
        site::wait_for_adds(std::time::Duration::from_secs(30));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Transparent, so the recording frame window can be see-through. Every
        // other window paints its own background.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.root_hidden_from_capture {
            self.root_hidden_from_capture = root_window(frame).is_some_and(platform::exclude_from_capture);
        }
        if !self.hidden_at_start {
            // eframe shows the root window after its first frame even when it was
            // built invisible; hide it until a capture starts.
            self.hidden_at_start = true;
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        self.handle_events(&ctx);

        // Test hook: open an image file in the editor at start.
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os("MYTHICSHOT_TEST_OPEN") {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !DONE.swap(true, Ordering::Relaxed) {
                if let Ok(img) = image::open(&path) {
                    self.open_editor(img.to_rgba8(), "Test image".into());
                }
            }
        }

        // Test hook: open Preferences the way the tray does, N ms after start.
        #[cfg(debug_assertions)]
        if let Some(ms) = std::env::var("MYTHICSHOT_TEST_PREFS_MS").ok().and_then(|v| v.parse::<u128>().ok()) {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if self.started.elapsed().as_millis() > ms && !DONE.swap(true, Ordering::Relaxed) {
                self.run(&ctx, Command::Preferences);
            }
        }

        if let Some(ov) = self.overlay.as_mut() {
            match ov.ui(ui) {
                Outcome::Continue => {}
                outcome => self.finish_overlay(&ctx, outcome),
            }
        } else if let Some(action) = self.session.as_ref().and_then(|s| s.bar_ui(ui, self.settings.record_keys)) {
            self.recording_action(&ctx, action);
        }
        self.tick_recording(&ctx);
        self.show_editors(&ctx);
        self.show_pickers(&ctx);
        self.toast.show(&ctx);
        self.main_window(&ctx);
        // Keep the event loop ticking slowly while hidden so queued events are never stranded.
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

/// The root window's native handle.
#[cfg(windows)]
fn root_window(frame: &eframe::Frame) -> Option<platform::WindowId> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
fn root_window(_frame: &eframe::Frame) -> Option<platform::WindowId> {
    None
}

/// Write panics to `crash.log` next to the settings file; release builds have
/// no console to show them.
fn install_crash_log() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let path = Settings::path().with_file_name("crash.log");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let backtrace = std::backtrace::Backtrace::force_capture();
        let text = format!("{}\n{info}\n{backtrace}\n\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
        default(info);
    }));
}

fn main() -> eframe::Result {
    install_crash_log();
    // Steps the installer runs; these never start the app.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--installer") {
        platform::installer_step(args.get(i + 1).map(String::as_str).unwrap_or(""));
        return Ok(());
    }
    // Debug builds can run beside an installed copy for tests.
    let second_allowed = cfg!(debug_assertions) && std::env::var_os("MYTHICSHOT_TEST_SECOND_COPY").is_some();
    if !second_allowed && !platform::single_instance() {
        eprintln!("Mythicshot is already running");
        return Ok(());
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: ViewportBuilder::default()
            .with_title("Mythicshot")
            .with_visible(false)
            .with_decorations(false)
            .with_taskbar(false)
            .with_always_on_top()
            // eframe's OpenGL backend picks one pixel format for every window from
            // the root; it must have alpha for the recording frame to be see-through.
            .with_transparent(true)
            .with_resizable(false)
            .with_position([-32000.0, -32000.0])
            .with_inner_size([1.0, 1.0]),
        run_and_return: false,
        ..Default::default()
    };
    eframe::run_native("Mythicshot", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
