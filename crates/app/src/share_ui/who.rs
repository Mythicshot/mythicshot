//! "Who can view": the choice "Publish image", "Publish recording" and "Publish tour" make when
//! something is first put online, its sentences, the checks made before anything is sent, and
//! how it is drawn. What goes over the wire is [`crate::publish::access`]'s alone.
//!
//! The choice is made only when something is created: nothing here changes who can view
//! something that is already online (that is done in the dashboard). It is never remembered:
//! every window starts at "Anyone with the link", and the addresses typed are overwritten when
//! the window goes. They are never part of a `Debug` print, a status line or a log line.

use std::fmt;

use eframe::egui::{self, Color32, RichText};

use crate::publish::access::{self, Access, Granted, Refused, Viewers};

pub const LABEL: &str = "Who can view";
pub const ADDRESSES_LABEL: &str = "Email addresses";
pub const PEOPLE_HELP: &str = "One per line. Each person gets a one-time link by email when they open it. There is no password.";
/// Under the choice "Only me". The wording is proposed and waits for the owner's approval.
pub const ONLY_ME: &str = "Only you can open it, signed in to the dashboard.";
/// Under the choice when the site is older than 0.1.51 or tells no version, and only "Anyone with
/// the link" is offered. The wording is proposed and waits for the owner's approval.
pub const TOO_OLD: &str = "This site is too old to restrict who can view. Update it to choose.";
/// "Publish tour", for a tour that is already published. The wording is proposed and waits for
/// the owner's approval.
pub const CHANGE_IN_DASHBOARD: &str = "This tour is already published, so who can view is changed in the dashboard.";
/// Why Publish waits: no address yet. The wording is proposed and waits for the owner's approval.
pub const NO_ADDRESS: &str = "Add at least one email address.";
/// Why Publish waits: more addresses than a site takes.
pub const TOO_MANY: &str = "At most 200 addresses.";
/// The end of a refusal of an image or a clip.
pub const NOTHING_SHARED: &str = "Nothing was shared.";
/// The end of a refusal of a tour. The wording is proposed and waits for the owner's approval.
pub const NOTHING_PUBLISHED: &str = "Nothing was published.";
/// An image or a clip the site created without what was asked; it was deleted before anything
/// was sent. The wording is proposed and waits for the owner's approval.
pub const NOT_APPLIED: &str = "The site did not apply who can view, so nothing was shared.";
/// A tour whose publish the site started without what was asked; it was not driven on. The
/// wording is proposed and waits for the owner's approval.
pub const TOUR_NOT_APPLIED: &str = "The site did not apply who can view, so the tour was not published.";
/// The same, when the site finished the publish at once and taking it down again failed. The
/// wording is proposed and waits for the owner's approval.
pub const TOUR_NOT_APPLIED_PUBLIC: &str = "The site did not apply who can view, and the tour is public now. Unpublish it on the Tours page.";
/// The result screen's last words for a restricted tour. The wording is proposed and waits for the owner's approval.
pub const ONLY_PEOPLE_WORDS: &str = "only the people you chose";
pub const ONLY_ME_WORDS: &str = "only you";

/// The three choices, in the order the dropdown has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    #[default]
    Anyone,
    People,
    OnlyMe,
}

impl Choice {
    pub const ALL: [Choice; 3] = [Choice::Anyone, Choice::People, Choice::OnlyMe];

    pub fn label(self) -> &'static str {
        match self {
            Choice::Anyone => "Anyone with the link",
            Choice::People => "Only people I choose",
            Choice::OnlyMe => "Only me",
        }
    }
}

/// "Address 3 is not an email address.": the third address (blank lines not counted). The
/// wording is proposed and waits for the owner's approval.
pub fn bad_address(n: usize) -> String {
    format!("Address {n} is not an email address.")
}

/// The line under the link of something restricted: "Only the 2 people you chose can open it.
/// Change this in the dashboard."
pub fn online_line(g: Granted) -> String {
    match g {
        Granted::People(1) => "Only the 1 person you chose can open it. Change this in the dashboard.".to_string(),
        Granted::People(n) => format!("Only the {n} people you chose can open it. Change this in the dashboard."),
        Granted::Me => "Only you can open it. Change this in the dashboard.".to_string(),
    }
}

/// What was refused: an image or a clip ([`NOTHING_SHARED`]), or a tour ([`NOTHING_PUBLISHED`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Shared,
    Published,
}

impl Ending {
    fn words(self) -> &'static str {
        match self {
            Ending::Shared => NOTHING_SHARED,
            Ending::Published => NOTHING_PUBLISHED,
        }
    }
}

/// The one sentence for a refusal of who can view, about a `noun` ("image", "clip", "tour").
/// Those for a site too old (413 to a padded request), `bad_viewer` without a number, `bad_access`, `bad_request`, `already_published`
/// and `access_already_set` are proposed and wait for the owner's approval.
pub fn refused_text(r: Refused, noun: &str, ending: Ending) -> String {
    let nothing = ending.words();
    match r {
        Refused::CannotRestrict => format!("This site cannot send mail, so it cannot restrict who can view. {nothing}"),
        Refused::TooLargeToRestrict => format!("This {noun} is too large to restrict on this site. {nothing}"),
        Refused::BadViewer(Some(n)) => format!("{} {nothing}", bad_address(n as usize)),
        Refused::BadViewer(None) => format!("One of the addresses is not an email address. {nothing}"),
        Refused::TooManyViewers => format!("{TOO_MANY} {nothing}"),
        Refused::BadAccess | Refused::BadRequest => format!("The site did not accept who can view. {nothing}"),
        Refused::AlreadyPublished => format!("This {noun} was published before, so who can view can only be changed in the dashboard. {nothing}"),
        Refused::AccessAlreadySet => format!("Who can view was already set for this {noun} in the dashboard. {nothing}"),
        Refused::SiteTooOld => format!("This site is too old to restrict who can view. {nothing}"),
        Refused::NotApplied => match ending {
            Ending::Shared => NOT_APPLIED.to_string(),
            Ending::Published => TOUR_NOT_APPLIED.to_string(),
        },
    }
}

/// Why the addresses typed cannot be sent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    NoAddress,
    TooMany,
    /// The address at this position (from 1, blank lines not counted) does not look like one.
    Bad(usize),
}

impl Problem {
    pub fn text(self) -> String {
        match self {
            Problem::NoAddress => NO_ADDRESS.to_string(),
            Problem::TooMany => TOO_MANY.to_string(),
            Problem::Bad(n) => bad_address(n),
        }
    }
}

/// Whether `s` looks like an address: exactly one "@", with text on both sides, and no space.
/// The site is the judge; this only keeps the common mistakes from reaching it.
fn looks_like_address(s: &str) -> bool {
    let mut parts = s.split('@');
    let (Some(name), Some(host), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    !name.is_empty() && !host.is_empty() && !s.chars().any(char::is_whitespace)
}

/// The addresses in `text`, one per line, trimmed, blank lines left out; or why they cannot be sent.
pub fn check(text: &str) -> Result<Vec<String>, Problem> {
    let list: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if list.is_empty() {
        return Err(Problem::NoAddress);
    }
    if list.len() > access::MAX_VIEWERS {
        return Err(Problem::TooMany);
    }
    if let Some(i) = list.iter().position(|a| !looks_like_address(a)) {
        return Err(Problem::Bad(i + 1));
    }
    Ok(list.into_iter().map(str::to_string).collect())
}

/// One piece of the choice as a window shows it, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// "Who can view" over the dropdown: the choices offered, and the one chosen.
    Choice { offered: Vec<Choice>, chosen: Choice },
    /// "Email addresses" over the field.
    Addresses,
    /// A small grey line: the help under the field.
    Help(&'static str),
    /// A grey line: what "Only me" means, why only one choice is offered, why Publish waits.
    Muted(String),
}

/// What the site said about itself, as far as who can view goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Site {
    /// Not known (yet): only "Anyone with the link" is offered, and nothing is said.
    #[default]
    Unknown,
    /// 0.1.51 or newer.
    Takes,
    /// Older, or it tells no version.
    TooOld,
}

impl Site {
    /// From the version `GET /api/v1/me` gave (None: the site tells none).
    pub fn of(version: Option<&str>) -> Site {
        if access::supported(version) { Site::Takes } else { Site::TooOld }
    }
}

/// The choice in a window: what is chosen and the addresses typed.
#[derive(Default)]
pub struct Form {
    choice: Choice,
    /// What is typed in the field, one address per line.
    pub addresses: String,
    site: Site,
}

impl fmt::Debug for Form {
    /// Never the addresses: only how many lines there are.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Form").field("choice", &self.choice).field("lines", &self.addresses.lines().count()).field("site", &self.site).finish()
    }
}

impl Drop for Form {
    fn drop(&mut self) {
        crate::publish_ui::wipe(&mut self.addresses);
    }
}

impl Form {
    pub fn set_site(&mut self, site: Site) {
        self.site = site;
    }

    /// The choices the dropdown offers: all three on a site that takes them, else only the first.
    pub fn offered(&self) -> Vec<Choice> {
        match self.site {
            Site::Takes => Choice::ALL.to_vec(),
            Site::Unknown | Site::TooOld => vec![Choice::Anyone],
        }
    }

    /// What is chosen: anything not offered counts as "Anyone with the link".
    pub fn chosen(&self) -> Choice {
        if self.offered().contains(&self.choice) { self.choice } else { Choice::Anyone }
    }

    pub fn choose(&mut self, choice: Choice) {
        self.choice = choice;
    }

    /// Why Publish waits, when it does.
    pub fn problem(&self) -> Option<Problem> {
        match self.chosen() {
            Choice::People => check(&self.addresses).err(),
            Choice::Anyone | Choice::OnlyMe => None,
        }
    }

    /// What is sent; or why it cannot be.
    pub fn access(&self) -> Result<Access, Problem> {
        match self.chosen() {
            Choice::Anyone => Ok(Access::Public),
            Choice::OnlyMe => Ok(Access::Me),
            Choice::People => check(&self.addresses).map(|list| Access::People(Viewers::new(list))),
        }
    }

    /// What the window shows of the choice, in order.
    pub fn parts(&self) -> Vec<Part> {
        let chosen = self.chosen();
        let mut out = vec![Part::Choice { offered: self.offered(), chosen }];
        if self.site == Site::TooOld {
            out.push(Part::Muted(TOO_OLD.to_string()));
        }
        match chosen {
            Choice::Anyone => {}
            Choice::People => {
                out.push(Part::Addresses);
                out.push(Part::Help(PEOPLE_HELP));
                out.extend(self.problem().map(|p| Part::Muted(p.text())));
            }
            Choice::OnlyMe => out.push(Part::Muted(ONLY_ME.to_string())),
        }
        out
    }
}

// ---- Drawing ----

/// The address field's id, so a window can tell that Enter is typed into it.
pub fn addresses_id() -> egui::Id {
    egui::Id::unique("who-can-view-addresses")
}

/// Whether the address field has the keyboard: Enter is then a new line, not the main button.
pub fn typing_addresses(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.has_focus(addresses_id()))
}

/// Draw one piece of the choice, as the app draws a labelled setting: the label over a
/// dropdown like the Preferences ones, the label over a three-row field.
pub fn draw_part(ui: &mut egui::Ui, part: &Part, form: &mut Form, muted: Color32) {
    match part {
        Part::Choice { offered, chosen } => {
            ui.label(RichText::new(LABEL).strong());
            let mut choice = *chosen;
            egui::ComboBox::from_id_salt("who-can-view").width(220.0).selected_text(chosen.label()).show_ui(ui, |ui| {
                for c in offered {
                    ui.selectable_value(&mut choice, *c, c.label());
                }
            });
            if choice != *chosen {
                form.choose(choice);
            }
        }
        Part::Addresses => {
            ui.label(RichText::new(ADDRESSES_LABEL).strong());
            ui.add(egui::TextEdit::multiline(&mut form.addresses).id(addresses_id()).desired_rows(3).desired_width(f32::INFINITY));
        }
        Part::Help(t) => {
            ui.add(egui::Label::new(RichText::new(*t).color(muted).size(12.0)).wrap());
        }
        Part::Muted(t) => {
            ui.add(egui::Label::new(RichText::new(t).color(muted)).wrap());
        }
    }
}

/// The small lock and a line beside it: who can open what was just put online.
pub fn lock_line(ui: &mut egui::Ui, text: &str, muted: Color32) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        crate::icons::paint(ui.painter(), rect, crate::icons::Icon::Lock, muted);
        ui.add(egui::Label::new(RichText::new(text).color(muted)).wrap());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sentences_of_the_mockup_are_word_for_word() {
        assert_eq!(LABEL, "Who can view");
        assert_eq!(Choice::ALL.map(Choice::label), ["Anyone with the link", "Only people I choose", "Only me"]);
        assert_eq!(Choice::default(), Choice::Anyone);
        assert_eq!(ADDRESSES_LABEL, "Email addresses");
        assert_eq!(PEOPLE_HELP, "One per line. Each person gets a one-time link by email when they open it. There is no password.");
        assert_eq!(online_line(Granted::People(2)), "Only the 2 people you chose can open it. Change this in the dashboard.");
        assert_eq!(online_line(Granted::People(1)), "Only the 1 person you chose can open it. Change this in the dashboard.");
        assert_eq!(online_line(Granted::Me), "Only you can open it. Change this in the dashboard.");
    }

    #[test]
    fn the_new_sentences_are_pinned() {
        assert_eq!(ONLY_ME, "Only you can open it, signed in to the dashboard.");
        assert_eq!(TOO_OLD, "This site is too old to restrict who can view. Update it to choose.");
        assert_eq!(CHANGE_IN_DASHBOARD, "This tour is already published, so who can view is changed in the dashboard.");
        assert_eq!(NO_ADDRESS, "Add at least one email address.");
        assert_eq!(TOO_MANY, "At most 200 addresses.");
        assert_eq!(bad_address(3), "Address 3 is not an email address.");
        assert_eq!(NOT_APPLIED, "The site did not apply who can view, so nothing was shared.");
        assert_eq!(TOUR_NOT_APPLIED, "The site did not apply who can view, so the tour was not published.");
        assert_eq!(TOUR_NOT_APPLIED_PUBLIC, "The site did not apply who can view, and the tour is public now. Unpublish it on the Tours page.");
        assert_eq!((ONLY_PEOPLE_WORDS, ONLY_ME_WORDS), ("only the people you chose", "only you"));
    }

    #[test]
    fn every_refusal_is_one_plain_sentence() {
        let s = |r, noun, e| refused_text(r, noun, e);
        assert_eq!(s(Refused::CannotRestrict, "image", Ending::Shared), "This site cannot send mail, so it cannot restrict who can view. Nothing was shared.");
        assert_eq!(s(Refused::TooLargeToRestrict, "clip", Ending::Shared), "This clip is too large to restrict on this site. Nothing was shared.");
        assert_eq!(s(Refused::BadViewer(Some(4)), "image", Ending::Shared), "Address 4 is not an email address. Nothing was shared.");
        assert_eq!(s(Refused::BadViewer(None), "image", Ending::Shared), "One of the addresses is not an email address. Nothing was shared.");
        assert_eq!(s(Refused::TooManyViewers, "image", Ending::Shared), "At most 200 addresses. Nothing was shared.");
        assert_eq!(s(Refused::BadAccess, "image", Ending::Shared), "The site did not accept who can view. Nothing was shared.");
        assert_eq!(s(Refused::BadRequest, "image", Ending::Shared), "The site did not accept who can view. Nothing was shared.");
        assert_eq!(s(Refused::NotApplied, "image", Ending::Shared), NOT_APPLIED);
        assert_eq!(s(Refused::SiteTooOld, "image", Ending::Shared), "This site is too old to restrict who can view. Nothing was shared.");
        assert_eq!(s(Refused::SiteTooOld, "tour", Ending::Published), "This site is too old to restrict who can view. Nothing was published.");
        assert_eq!(s(Refused::NotApplied, "tour", Ending::Published), TOUR_NOT_APPLIED);
        assert_eq!(s(Refused::CannotRestrict, "tour", Ending::Published), "This site cannot send mail, so it cannot restrict who can view. Nothing was published.");
        assert_eq!(s(Refused::AlreadyPublished, "tour", Ending::Published), "This tour was published before, so who can view can only be changed in the dashboard. Nothing was published.");
        assert_eq!(s(Refused::AccessAlreadySet, "tour", Ending::Published), "Who can view was already set for this tour in the dashboard. Nothing was published.");
    }

    #[test]
    fn the_common_mistakes_never_reach_the_site() {
        assert_eq!(check(""), Err(Problem::NoAddress));
        assert_eq!(check("  \n\n \t"), Err(Problem::NoAddress));
        assert_eq!(check(" a@example.com \n\nb@example.org\n"), Ok(vec!["a@example.com".to_string(), "b@example.org".to_string()]));
        for bad in ["no-at.example.com", "@example.com", "name@", "a@b@c", "a b@example.com", "a@exa mple.com"] {
            assert_eq!(check(&format!("ok@example.com\n\n{bad}")), Err(Problem::Bad(2)), "{bad}");
        }
        let many = |n: usize| (0..n).map(|i| format!("p{i}@example.com")).collect::<Vec<_>>().join("\n");
        assert_eq!(check(&many(200)).map(|l| l.len()), Ok(200));
        assert_eq!(check(&many(201)), Err(Problem::TooMany));
        assert_eq!(Problem::NoAddress.text(), NO_ADDRESS);
        assert_eq!(Problem::TooMany.text(), TOO_MANY);
        assert_eq!(Problem::Bad(2).text(), "Address 2 is not an email address.");
    }

    #[test]
    fn only_a_site_that_takes_it_is_offered_a_restriction() {
        let mut f = Form::default();
        assert_eq!(f.offered(), vec![Choice::Anyone]);
        assert_eq!(f.parts(), vec![Part::Choice { offered: vec![Choice::Anyone], chosen: Choice::Anyone }], "nothing is said while the site is asked");
        f.set_site(Site::of(Some("0.1.50")));
        assert_eq!(f.parts(), vec![Part::Choice { offered: vec![Choice::Anyone], chosen: Choice::Anyone }, Part::Muted(TOO_OLD.into())]);
        f.set_site(Site::of(None));
        assert_eq!(f.site, Site::TooOld);
        // A choice that is not offered counts as anyone, and sends nothing.
        f.choose(Choice::OnlyMe);
        assert_eq!((f.chosen(), f.access()), (Choice::Anyone, Ok(Access::Public)));
        f.set_site(Site::of(Some("0.1.51")));
        assert_eq!(f.offered(), Choice::ALL.to_vec());
        assert_eq!(f.access(), Ok(Access::Me));
        assert_eq!(f.parts(), vec![Part::Choice { offered: Choice::ALL.to_vec(), chosen: Choice::OnlyMe }, Part::Muted(ONLY_ME.into())]);
    }

    #[test]
    fn people_needs_addresses_and_says_why_it_waits() {
        let mut f = Form::default();
        f.set_site(Site::Takes);
        f.choose(Choice::People);
        assert_eq!(f.problem(), Some(Problem::NoAddress));
        assert_eq!(f.parts(), vec![Part::Choice { offered: Choice::ALL.to_vec(), chosen: Choice::People }, Part::Addresses, Part::Help(PEOPLE_HELP), Part::Muted(NO_ADDRESS.into())]);
        f.addresses = "a@example.com\nb@example.com".into();
        assert_eq!(f.problem(), None);
        assert_eq!(f.access(), Ok(Access::People(Viewers::new(vec!["a@example.com".into(), "b@example.com".into()]))));
        assert_eq!(f.parts().len(), 3);
    }

    #[test]
    fn the_debug_print_of_the_choice_never_holds_an_address() {
        let mut f = Form::default();
        f.set_site(Site::Takes);
        f.choose(Choice::People);
        f.addresses = "private.person@example.com\nother@example.org".into();
        let text = format!("{f:?} {:?} {:?}", f.access(), f.parts());
        assert!(!text.contains("private.person") && !text.contains('@'), "{text}");
    }
}
