//! Saved projects: the recording and edit state of an uploaded tour, kept on this computer so the
//! tour can be opened and updated later. No UI lives here.
//!
//! [`state`] is the on-disk format (`project.json`) and its conversions to and from the editor's types.
//! [`store`] is the folder the projects live in: save, list, open, remove.

mod convert;
mod state;
mod store;
mod usage;

pub use state::{COVER_LOGO, ProjectKey, Restored, Snapshot};
pub use store::{LoadError, Loaded, NOT_PUT_BACK, OpenGuard, Problem, ProjectInfo, Saved, Store, Totals, epoch};
// What the saved recordings take, in words: for the settings and for Preferences.
pub use usage::{LIMIT_MAX_GB, form_text, over_limit, over_limit_sentence, parse_limit, problem_text, remove_question, row_title, usage_fraction, usage_line};
// For the publish result screen: whether a save went past the limit.
pub use usage::pushed_over;
// The form a recording is kept in and how storing it as video went, and what removing one
// answers: Preferences shows them.
pub use store::{ConvertState, Form, Removed, converting_now};
// How a project that keeps its recording as a video is opened: the Tours page's Edit asks.
pub use store::Expand;
// The conversion queue: what the app tells it, and what the pages ask it.
pub use convert::{FfmpegState, MIN_BYTES, ffmpeg_state, gate, nudge, set_enabled, set_recording, shutdown, start};
// The room left on a folder's disk: asked before a video is expanded for an editor.
pub(crate) use convert::free_bytes;
#[cfg(test)]
pub use convert::nudges_here;

// Named outside this module by tests only: the format's number, two of the store's reasons,
// one conversion on its own and how it ended, and what the queue tells and asks the store.
#[cfg(test)]
pub use convert::{Outcome, convert_one};
#[cfg(test)]
pub use state::FORMAT;
#[cfg(test)]
pub use store::{DISK_FULL, SAVING_CANCELLED, cancel_conversion_of, set_current_codec};
