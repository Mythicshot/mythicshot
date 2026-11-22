//! "Add to site": the dialog that picks a site folder, the site's title and the tour's address.
//!
//! The folder is looked at on a background thread, so a slow or sleeping drive never holds up the editor.
//! While the dialog is open it takes Escape and Enter and the editor behind it is inert, as with the
//! "Publish tour" dialog.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, RichText};

use crate::theme;
use crate::publish_ui::short_title;
use crate::site::{self, Folder};


/// The approved line under the title.
pub const INTRO: &str = "Writes this tour into a folder of plain web files. Host that folder anywhere, such as GitHub Pages.";
/// The approved note when the address holds no tour yet.
pub const NOTE: &str = "A tour already at this address will be replaced. Other tours in the site are not touched.";
/// Longest tour title the note names before cutting it with an ellipsis.
const NOTE_TITLE_MAX: usize = 60;

/// What the user chose in the dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum SiteOutcome {
    Cancel,
    Add { folder: PathBuf, title: String, slug: String },
}

/// The line under the address: what "Add to site" will do, or why it cannot.
pub fn address_note(folder_state: &Result<Folder, String>, address: &str, _tour_title: &str) -> Result<String, String> {
    let folder = folder_state.as_ref().map_err(|e| e.clone())?;
    let slug = site::parse_address(address)?;
    if let Folder::Site(list) = folder
        && let Some(t) = list.tours.iter().find(|t| t.slug == slug)
    {
        let steps = if t.steps == 1 { "1 step".to_string() } else { format!("{} steps", t.steps) };
        return Ok(format!("This replaces \u{201c}{}\u{201d} ({steps}). Other tours in the site are not touched.", short_title(&t.title, NOTE_TITLE_MAX)));
    }
    Ok(NOTE.to_string())
}

/// What the background look at the folder found.
#[derive(Debug, Clone, PartialEq)]
enum Check {
    /// No folder chosen.
    None,
    Checking,
    /// The folder, whether it exists, and what [`site::inspect`] said.
    Done(PathBuf, bool, Result<Folder, String>),
}

/// The "Add to site" dialog.
pub struct SiteDialog {
    folder: Option<PathBuf>,
    title: String,
    address: String,
    tour_title: String,
    /// The user typed in these since the folder was chosen, so a site's own values do not replace them.
    title_edited: bool,
    address_edited: bool,
    check: Arc<Mutex<Check>>,
    /// What this dialog has taken from the last finished check.
    seen: Option<Result<Folder, String>>,
    /// The folder being looked at is the remembered one, which may have gone since.
    remembered: bool,
    /// The folder used last time is no longer there: its name.
    gone: Option<String>,
    /// Why the last "Add to site" could not start.
    refused: Option<String>,
}

impl SiteDialog {
    fn new(folder: &str, site_title: &str, tour_title: &str) -> SiteDialog {
        let folder = (!folder.trim().is_empty()).then(|| PathBuf::from(folder.trim()));
        let address = site::address(&site::suggest_slug(tour_title, None));
        SiteDialog { folder, title: site_title.to_string(), address, tour_title: tour_title.to_string(), title_edited: false, address_edited: false, check: Arc::new(Mutex::new(Check::None)), seen: None, remembered: false, gone: None, refused: None }
    }

    /// The dialog with the remembered folder and site title, looking at the folder in the background.
    pub fn open(folder: &str, site_title: &str, tour_title: &str) -> SiteDialog {
        // Test hook: start with this folder, in memory only.
        #[cfg(debug_assertions)]
        let test_folder = std::env::var("MYTHICSHOT_TEST_SITE").ok();
        #[cfg(not(debug_assertions))]
        let test_folder: Option<String> = None;
        let mut d = SiteDialog::new(test_folder.as_deref().unwrap_or(folder), site_title, tour_title);
        if let Some(f) = d.folder.clone() {
            // The hook's folder may not exist yet; only a remembered one is reported as gone.
            d.look_at(f, test_folder.is_none());
        }
        d
    }

    /// Keep the dialog open and say why the add did not start.
    pub fn refuse(&mut self, why: String) {
        self.refused = Some(why);
    }

    /// Look at `folder` on a background thread; whatever an older look finds is dropped.
    fn look_at(&mut self, folder: PathBuf, remembered: bool) {
        self.remembered = remembered;
        self.folder = Some(folder.clone());
        self.seen = None;
        self.refused = None;
        self.title_edited = false;
        let check = Arc::new(Mutex::new(Check::Checking));
        self.check = check.clone();
        std::thread::spawn(move || {
            let exists = folder.symlink_metadata().is_ok();
            let found = site::check_dedicated(&folder).and_then(|_| site::inspect(&folder));
            if let Ok(mut c) = check.lock() {
                *c = Check::Done(folder, exists, found);
            }
        });
    }

    /// Take in a finished look: a site's own title and the address it offers, unless the user typed them.
    fn take_check(&mut self) -> bool {
        let state = self.check.lock().map(|c| c.clone()).unwrap_or(Check::None);
        match state {
            Check::None => false,
            Check::Checking => true,
            Check::Done(folder, exists, found) => {
                if self.seen.is_none() {
                    // The folder used last time has gone: ask for one rather than make it again somewhere unexpected.
                    if !exists && self.remembered {
                        self.gone = Some(folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.display().to_string()));
                        self.folder = None;
                        self.check = Arc::new(Mutex::new(Check::None));
                        return false;
                    }
                    if let Ok(Folder::Site(list)) = &found {
                        if !self.title_edited {
                            self.title = list.title.clone();
                        }
                        if !self.address_edited {
                            self.address = site::address(&site::suggest_slug(&self.tour_title, Some(list)));
                        }
                    }
                    self.seen = Some(found);
                }
                false
            }
        }
    }

    /// The note under the address, or why "Add to site" cannot be used yet.
    fn note(&self) -> Result<String, String> {
        match (&self.folder, &self.seen) {
            (None, _) => Err("Choose a folder for the site.".to_string()),
            (Some(_), None) => Err("Checking the folder…".to_string()),
            (Some(_), Some(found)) => address_note(found, &self.address, &self.tour_title),
        }
    }

    fn outcome(&self) -> Option<SiteOutcome> {
        let folder = self.folder.clone()?;
        self.note().ok()?;
        let slug = site::parse_address(&self.address).ok()?;
        Some(SiteOutcome::Add { folder, title: site::site_title(&self.title), slug })
    }

    /// Draw the dialog; Some once the user cancels or chooses. Escape cancels and Enter is
    /// "Add to site" when it can be used; both keys are taken from the editor.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<SiteOutcome> {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            return Some(SiteOutcome::Cancel);
        }
        let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        if self.take_check() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let mut outcome = None;
        let mut choose = false;
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(22.0);
        crate::share_ui::modal(egui::Id::unique("add-to-site-dialog")).frame(frame).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading("Add to site");
            ui.add(egui::Label::new(RichText::new(INTRO).color(theme::MUTED)).wrap());
            ui.add_space(12.0);
            egui::Grid::new("add-to-site-rows").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label("Site folder");
                ui.horizontal(|ui| {
                    let choose_w = 80.0;
                    let shown = self.folder.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "No folder chosen".to_string());
                    ui.scope(|ui| {
                        ui.set_max_width((ui.available_width() - choose_w).max(60.0));
                        let color = if self.folder.is_some() { ui.visuals().text_color() } else { theme::MUTED };
                        let r = ui.add(egui::Label::new(RichText::new(&shown).color(color)).truncate());
                        if self.folder.is_some() {
                            r.on_hover_text(&shown);
                        }
                    });
                    if ui.button("Choose…").clicked() {
                        choose = true;
                    }
                });
                ui.end_row();
                ui.label("Site title");
                if ui.add(egui::TextEdit::singleline(&mut self.title).char_limit(site::SITE_TITLE_MAX).hint_text("Tours").desired_width(f32::INFINITY)).changed() {
                    self.title_edited = true;
                }
                ui.end_row();
                ui.label("Tour address");
                let before = self.address.clone();
                ui.add(egui::TextEdit::singleline(&mut self.address).hint_text("tours/my-tour/").desired_width(f32::INFINITY));
                if self.address != before {
                    self.address_edited = true;
                    self.refused = None;
                }
                ui.end_row();
            });
            ui.add_space(8.0);
            if let Some(name) = &self.gone {
                ui.add(egui::Label::new(RichText::new(format!("The folder used last time, {name}, is no longer there.")).color(theme::MUTED)).wrap());
            }
            match self.note() {
                Ok(note) => ui.add(egui::Label::new(RichText::new(note).color(theme::MUTED)).wrap()),
                Err(why) if self.folder.is_none() || self.seen.is_none() => ui.add(egui::Label::new(RichText::new(why).color(theme::MUTED)).wrap()),
                Err(why) => ui.add(egui::Label::new(RichText::new(why).color(theme::DANGER_TEXT)).wrap()),
            };
            if let Some(why) = &self.refused {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(why).color(theme::DANGER_TEXT)).wrap());
            }
            ui.add_space(16.0);
            let ready = self.outcome();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let add = theme::primary_button("Add to site");
                if ui.add_enabled(ready.is_some(), add).clicked() {
                    outcome = ready.clone();
                }
                if ui.button("Cancel").clicked() {
                    outcome = Some(SiteOutcome::Cancel);
                }
            });
        });
        if choose {
            let mut dialog = rfd::FileDialog::new().set_title("Site folder");
            if let Some(start) = self.folder.as_deref().and_then(existing_start) {
                dialog = dialog.set_directory(start);
            }
            if let Some(f) = dialog.pick_folder() {
                self.gone = None;
                self.look_at(f, false);
            }
        }
        if outcome.is_none() && enter {
            outcome = self.outcome();
        }
        outcome
    }
}

/// Where the folder picker starts: the folder, or its parent when the folder is not there yet.
fn existing_start(folder: &Path) -> Option<&Path> {
    if folder.is_dir() { Some(folder) } else { folder.parent().filter(|p| p.is_dir()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::list;

    #[test]
    fn the_note_says_what_will_happen() {
        let new: Result<Folder, String> = Ok(Folder::New);
        assert_eq!(address_note(&new, "tours/raid-groups/", "Raid groups").unwrap(), "A tour already at this address will be replaced. Other tours in the site are not touched.");
        let site: Result<Folder, String> = Ok(Folder::Site(list(&[("raid-groups", "Raid groups")])));
        assert_eq!(address_note(&site, "tours/raid-groups/", "Raid groups").unwrap(), "This replaces \u{201c}Raid groups\u{201d} (3 steps). Other tours in the site are not touched.");
        assert!(address_note(&site, "tours/other/", "Other").unwrap().starts_with("A tour already"));
        // One step, and a long title cut short.
        let mut l = list(&[("a", &"T".repeat(200))]);
        l.tours[0].steps = 1;
        let note = address_note(&Ok(Folder::Site(l)), "a", "A").unwrap();
        assert!(note.contains("(1 step)") && note.contains('…') && note.chars().count() < 140, "{note}");
    }

    #[test]
    fn a_bad_folder_or_address_gives_its_reason() {
        let bad: Result<Folder, String> = Err("index.html is already in this folder and was not made by Mythicshot".into());
        assert_eq!(address_note(&bad, "tours/a/", "A").unwrap_err(), "index.html is already in this folder and was not made by Mythicshot");
        let new: Result<Folder, String> = Ok(Folder::New);
        for a in ["tours/../x/", "tours/A/", "tours/assets/", "", "tours/tmp-a-1234abcd/", "C:/x", "tours/con/"] {
            assert!(address_note(&new, a, "A").is_err(), "{a}");
        }
    }

    #[test]
    fn old_settings_files_load_with_no_site() {
        let p: crate::settings::EditorPrefs = toml::from_str("tool = \"rectangle\"").unwrap();
        assert_eq!((p.site_folder.as_str(), p.site_title.as_str()), ("", ""));
        let mut s = crate::settings::Settings::default();
        s.editor.site_folder = "/sites/guides".into();
        s.editor.site_title = "Guides".into();
        let back: crate::settings::Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!((back.editor.site_folder.as_str(), back.editor.site_title.as_str()), ("/sites/guides", "Guides"));
    }

    fn key(k: egui::Key) -> egui::RawInput {
        egui::RawInput { events: vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], ..Default::default() }
    }

    /// Run frames until the background look at the folder has finished.
    fn settle(ctx: &egui::Context, d: &mut SiteDialog) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while d.seen.is_none() && d.folder.is_some() && std::time::Instant::now() < deadline {
            ctx.run_ui(egui::RawInput::default(), |ui| assert!(d.show(ui.ctx()).is_none())).textures_delta.clear();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn enter_adds_escape_cancels_and_nothing_starts_without_a_folder() {
        let ctx = crate::theme::test_context();
        let tmp = tempfile::tempdir().unwrap();
        // No folder: Enter does nothing.
        let mut d = SiteDialog::new("", "", "Raid groups");
        let mut out = None;
        ctx.run_ui(key(egui::Key::Enter), |ui| out = d.show(ui.ctx())).textures_delta.clear();
        assert!(out.is_none());
        assert_eq!(d.address, "tours/raid-groups/");
        // An empty folder: Enter adds, with the address made from the title and "Tours" for an empty title.
        let mut d = SiteDialog::new("", "", "Raid groups");
        d.look_at(tmp.path().to_path_buf(), false);
        settle(&ctx, &mut d);
        let mut out = None;
        ctx.run_ui(key(egui::Key::Enter), |ui| out = d.show(ui.ctx())).textures_delta.clear();
        assert_eq!(out, Some(SiteOutcome::Add { folder: tmp.path().to_path_buf(), title: "Tours".into(), slug: "raid-groups".into() }));
        let mut out = None;
        ctx.run_ui(key(egui::Key::Escape), |ui| out = d.show(ui.ctx())).textures_delta.clear();
        assert_eq!(out, Some(SiteOutcome::Cancel));
        // A bad address: Enter does nothing.
        d.address = "tours/../x/".into();
        let mut out = None;
        ctx.run_ui(key(egui::Key::Enter), |ui| out = d.show(ui.ctx())).textures_delta.clear();
        assert!(out.is_none());
        // Nothing was written by the dialog.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_site_brings_its_title_and_a_free_address() {
        let ctx = crate::theme::test_context();
        let tmp = tempfile::tempdir().unwrap();
        let mut l = list(&[("raid-groups", "Raid groups")]);
        l.title = "Raid guides".into();
        std::fs::write(tmp.path().join("tours.json"), serde_json::to_string(&l).unwrap()).unwrap();
        std::fs::create_dir_all(tmp.path().join("tours/raid-groups")).unwrap();
        // Another tour whose title gives the same slug is offered the next address.
        let mut d = SiteDialog::new(&tmp.path().to_string_lossy(), "Old title", "Raid: groups");
        d.look_at(tmp.path().to_path_buf(), false);
        settle(&ctx, &mut d);
        assert_eq!((d.title.as_str(), d.address.as_str()), ("Raid guides", "tours/raid-groups-2/"));
        assert_eq!(d.note().unwrap(), NOTE);
        // The same title is offered its own address, and the note names the tour it replaces.
        let mut d = SiteDialog::new("", "", "Raid groups");
        d.look_at(tmp.path().to_path_buf(), false);
        settle(&ctx, &mut d);
        assert_eq!(d.address, "tours/raid-groups/");
        assert!(d.note().unwrap().starts_with("This replaces \u{201c}Raid groups\u{201d}"));
    }

    #[test]
    fn a_main_folder_is_refused_in_the_dialog() {
        let ctx = crate::theme::test_context();
        let Some(user) = directories::UserDirs::new() else { return };
        let mut d = SiteDialog::new("", "", "A");
        d.look_at(user.home_dir().to_path_buf(), false);
        settle(&ctx, &mut d);
        assert!(d.note().unwrap_err().contains("folder just for the site"));
        assert!(d.outcome().is_none());
    }

    #[test]
    fn a_remembered_folder_that_is_gone_asks_for_another() {
        let ctx = crate::theme::test_context();
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("old-site");
        let mut d = SiteDialog::new(&gone.to_string_lossy(), "Guides", "A");
        d.look_at(gone.clone(), true);
        settle(&ctx, &mut d);
        for _ in 0..3 {
            ctx.run_ui(egui::RawInput::default(), |ui| assert!(d.show(ui.ctx()).is_none())).textures_delta.clear();
        }
        assert_eq!((d.folder.as_ref(), d.gone.as_deref()), (None, Some("old-site")));
        assert!(d.note().is_err() && d.outcome().is_none());
        assert_eq!(d.title, "Guides");
        assert!(!gone.exists());
    }
}
