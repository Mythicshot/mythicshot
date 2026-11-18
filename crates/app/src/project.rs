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
// The settings name the largest limit; the rest is for Preferences and the publish result screen.
#[allow(unused_imports)]
pub use usage::{ConvertState, Form, LIMIT_MAX_GB, form_text, over_limit, over_limit_sentence, parse_limit, pushed_over, remove_question, row_title, usage_fraction, usage_line};

// Named outside this module by tests only: the format's number, and two of the store's reasons.
#[cfg(test)]
pub use state::FORMAT;
#[cfg(test)]
pub use store::{DISK_FULL, SAVING_CANCELLED};
