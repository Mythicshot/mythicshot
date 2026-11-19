//! Saved projects: the recording and edit state of an uploaded tour, kept on this computer so the
//! tour can be opened and updated later. No UI lives here.
//!
//! [`state`] is the on-disk format (`project.json`) and its conversions to and from the editor's types.
//! [`store`] is the folder the projects live in: save, list, open, remove.

mod state;
mod store;
mod usage;

pub use state::{COVER_LOGO, ProjectKey, Restored, Snapshot};
pub use store::{LoadError, Loaded, NOT_PUT_BACK, OpenGuard, Problem, ProjectInfo, Saved, Store, Totals, epoch};
// What the saved recordings take, in words: for the settings and for Preferences.
pub use usage::{LIMIT_MAX_GB, form_text, over_limit, over_limit_sentence, parse_limit, problem_text, remove_question, row_title, usage_fraction, usage_line};
// For the publish result screen, which does not ask yet.
#[allow(unused_imports)]
pub use usage::{UNTITLED, pushed_over};
// The form a recording is kept in and how storing it as video went, and what removing one
// answers: Preferences shows them.
pub use store::{ConvertState, Form, Removed, converting_now};
// What the conversion queue and the Tours page ask the store; nothing outside this module asks yet.
#[allow(unused_imports)]
pub use store::{Candidate, Converting, Expand, LOAD_CANCELLED, LOAD_NEEDS_FFMPEG, LOAD_NO_ROOM, LOAD_VIDEO_DAMAGED, MARK_FAILED, MARK_NOT_SMALLER, cancel_conversion_of, set_current_codec};

// Named outside this module by tests only: the format's number, and two of the store's reasons.
#[cfg(test)]
pub use state::FORMAT;
#[cfg(test)]
pub use store::{DISK_FULL, SAVING_CANCELLED};
