//! Mythicshot: a tray app that captures the screen.
//!
//! The root eframe window is the region-selector overlay. It stays hidden
//! until a capture starts, then covers the whole desktop. Hotkeys, tray menu
//! clicks and raw key events (only while the settings page records a new
//! shortcut) arrive on one channel and wake the UI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod editor_ui;
mod hotkeys;
mod icons;
mod overlay;
mod platform;
mod popups;
mod settings;
mod settings_ui;
mod tray;

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
    Menu(tray_icon::menu::MenuId),
    TrayClick,
    Key(rdev::EventType),
}

struct App {
    settings: Settings,
    hotkeys: Hotkeys,
    tray: Tray,
    rx: Receiver<AppEvent>,
    tx: Sender<AppEvent>,
    overlay: Option<Overlay>,
    settings_open: bool,
    focus_settings: bool,
    settings_state: SettingsState,
    settings_shared: Arc<std::sync::Mutex<settings_ui::Shared>>,
    recorder: ShortcutRecorder,
    key_hook_started: bool,
    /// Open editors; the flag is true until the window has been shown once.
    editors: Vec<(editor_ui::EditorWindow, bool)>,
    icon: Arc<egui::IconData>,
    listening: Arc<AtomicBool>,
    snipping_tool: Option<bool>,
    pickers: Vec<popups::PickerWindow>,
    toast: popups::Toast,
    hidden_at_start: bool,
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    started: Instant,
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

        let settings = Settings::load();
        let mut hotkeys = Hotkeys::new();
        for a in Action::ALL {
            hotkeys.apply(*a, settings.binding(*a));
            match (hotkeys.errors.get(a), settings.binding(*a)) {
                (Some(e), b) => eprintln!("hotkey {} [{b}]: {e}", a.label()),
                (None, "") => eprintln!("hotkey {}: no shortcut", a.label()),
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
        let settings_shared = Arc::new(std::sync::Mutex::new(settings_ui::Shared {
            settings: settings.clone(),
            errors: HashMap::new(),
            snipping_tool: None,
            autostart: false,
            state: SettingsState::default(),
            actions: Vec::new(),
            close_requested: false,
            focus: false,
        }));

        Self {
            settings,
            hotkeys,
            tray,
            rx,
            tx,
            overlay: None,
            settings_open: std::env::args().any(|a| a == "--settings"),
            focus_settings: true,
            settings_state: SettingsState::default(),
            settings_shared,
            recorder: ShortcutRecorder::default(),
            key_hook_started: false,
            editors: Vec::new(),
            icon: Arc::new(egui::IconData { rgba: tray::icon_rgba(64).0, width: 64, height: 64 }),
            listening: Arc::new(AtomicBool::new(false)),
            snipping_tool: platform::snipping_tool_owns_prtsc(),
            pickers: Vec::new(),
            toast: popups::Toast::new(),
            hidden_at_start: false,
            started: Instant::now(),
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AppEvent::Hotkey(id) => {
                    if self.listening.load(Ordering::Relaxed) || self.overlay.is_some() {
                        continue;
                    }
                    if let Some(action) = self.hotkeys.action_for(id) {
                        self.run(ctx, Command::from_action(action));
                    }
                }
                AppEvent::Menu(id) => {
                    if let Some(cmd) = self.tray.command(&id) {
                        self.run(ctx, cmd);
                    }
                }
                AppEvent::TrayClick => self.run(ctx, Command::Region),
                AppEvent::Key(ev) => self.record_key(&ev),
            }
        }
    }

    fn run(&mut self, ctx: &egui::Context, cmd: Command) {
        match cmd {
            Command::Region => self.start_overlay(ctx),
            Command::LastRegion => match self.settings.last_region {
                Some([x, y, w, h]) => self.capture_now(ctx, |_| Some((PxRect::new(x, y, w as u32, h as u32), "Region".into()))),
                None => self.start_overlay(ctx),
            },
            Command::Window => self.capture_now(ctx, |d| d.focused_window().map(|w| (w.rect, w.title.clone()))),
            Command::FullScreen => self.capture_now(ctx, |d| Some((d.bounds, "Full screen".into()))),
            Command::Record => self.notify("GIF recording arrives in Phase 4".into()),
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
            Command::Preferences => {
                self.settings_open = true;
                self.focus_settings = true;
            }
            Command::OpenFolder => platform::open_folder(&self.settings.output_dir),
            Command::Exit => ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }

    fn start_overlay(&mut self, ctx: &egui::Context) {
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
        self.overlay = Some(Overlay::new(desktop, self.settings.magnifier));
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
            if let Some(img) = ov.desktop.crop(rect) {
                self.settings.last_region = Some([rect.x, rect.y, rect.w as i32, rect.h as i32]);
                self.settings.save();
                // A window picked in the overlay keeps its title for the file name.
                let title = ov.desktop.windows.iter().find(|w| w.rect == rect).map(|w| w.title.clone()).unwrap_or_else(|| "Region".into());
                self.captured(ctx, img, title);
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
        let editor = editor_ui::Editor::new(img, title.clone(), self.settings.editor_keys.clone(), self.settings.dest_context(&title));
        self.editors.push((editor_ui::EditorWindow::new(editor), true));
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
        self.editors.retain(|(w, _)| !w.is_closed());
        for (w, first) in &mut self.editors {
            w.show(ctx, self.icon.clone(), std::mem::take(first));
        }
    }

    fn notify(&mut self, msg: String) {
        eprintln!("{msg}");
        self.toast.post(msg, None);
    }

    fn settings_changed(&mut self) {
        self.settings.save();
        self.tray.sync(&self.settings);
        for (w, _) in &self.editors {
            if let Ok(mut e) = w.editor.lock() {
                e.keys = self.settings.editor_keys.clone();
                e.set_output(self.settings.dest_context(""));
            }
        }
    }

    fn set_binding(&mut self, bind: Bind, binding: String) {
        match bind {
            Bind::Global(action) => {
                self.settings.hotkeys.insert(action, binding.clone());
                self.hotkeys.apply(action, &binding);
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

    fn listen(&mut self, ctx: &egui::Context, action: Option<Bind>) {
        self.settings_state.listening = action;
        self.recorder = ShortcutRecorder::default();
        self.listening.store(action.is_some(), Ordering::Relaxed);
        if action.is_some() && !self.key_hook_started {
            self.key_hook_started = true;
            let (tx, flag, ctx) = (self.tx.clone(), self.listening.clone(), ctx.clone());
            std::thread::spawn(move || {
                // The hook only forwards events while the settings page is listening.
                let result = rdev::listen(move |e| {
                    if flag.load(Ordering::Relaxed) {
                        let _ = tx.send(AppEvent::Key(e.event_type));
                        ctx.request_repaint();
                    }
                });
                if let Err(e) = result {
                    eprintln!("keyboard hook unavailable: {e:?}");
                }
            });
        }
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

    fn settings_window(&mut self, ctx: &egui::Context) {
        let builder = ViewportBuilder::default()
            .with_title("Mythicshot settings")
            .with_inner_size([860.0, 560.0])
            .with_min_inner_size([640.0, 420.0])
            .with_icon(self.icon.clone());
        // Pull what the settings window did since our last frame.
        let (actions, close, edited, tab) = {
            let mut s = self.settings_shared.lock().expect("settings state");
            (std::mem::take(&mut s.actions), std::mem::take(&mut s.close_requested), s.settings.clone(), s.state.tab)
        };
        self.settings_state.tab = tab;
        for a in actions {
            match a {
                UiAction::Listen(action) => self.listen(ctx, Some(action)),
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
                    // Only the General tab edits fields directly; shortcuts go through SetBinding.
                    self.settings.output_dir = edited.output_dir.clone();
                    self.settings.destinations = edited.destinations.clone();
                    self.settings.filename_pattern = edited.filename_pattern.clone();
                    self.settings.image_format = edited.image_format;
                    self.settings.jpeg_quality = edited.jpeg_quality;
                    self.settings.magnifier = edited.magnifier;
                    self.settings_changed();
                }
                UiAction::SetAutostart(on) => {
                    if let Err(e) = platform::set_autostart(on) {
                        self.notify(e);
                    }
                }
            }
        }
        if close {
            self.stop_listening();
            self.settings_open = false;
            return;
        }

        // Push the current state back and keep the window drawn.
        {
            let mut s = self.settings_shared.lock().expect("settings state");
            s.settings = self.settings.clone();
            s.errors = self.binding_errors();
            s.snipping_tool = self.snipping_tool;
            s.autostart = platform::autostart_enabled();
            s.state.listening = self.settings_state.listening;
            s.focus |= std::mem::take(&mut self.focus_settings);
        }
        settings_ui::show(ctx, &self.settings_shared, builder);
        ctx.request_repaint_of(settings_ui::viewport_id());
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
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
        }
        self.show_editors(&ctx);
        self.show_pickers(&ctx);
        self.toast.show(&ctx);
        if self.settings_open {
            self.settings_window(&ctx);
        }
        // Keep the event loop ticking slowly while hidden so queued events are never stranded.
        ctx.request_repaint_after(Duration::from_millis(250));
    }
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
    if !platform::single_instance() {
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
            .with_resizable(false)
            .with_position([-32000.0, -32000.0])
            .with_inner_size([1.0, 1.0]),
        run_and_return: false,
        ..Default::default()
    };
    eframe::run_native("Mythicshot", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
