//! Saved projects: the recording and edit state of an uploaded tour, kept on this computer so the
//! tour can be opened and updated later. No UI lives here.
//!
//! [`state`] is the on-disk format (`project.json`) and its conversions to and from the editor's types.
//! [`store`] is the folder the projects live in: save, list, open, remove.

// Publishing saves through the store and the Tours page lists, opens and removes. What is left
// unused outside the tests: the totals and the folder itself, which Preferences shows, the
// whole-store list, a save that cannot be cancelled, the one-tour list, and the format's own
// names, which only the tests name.
#![allow(dead_code, unused_imports)]

mod state;
mod store;

pub use state::{Blob, FORMAT, ItemEntry, MAX_PROJECT_JSON_BYTES, ParseError, ProjectFile, ProjectKey, Restored, SavedEdit, SavedEvent, SavedItem, Snapshot, from_file, parse, to_file, to_text};
pub use store::{DISK_FULL, LoadError, Loaded, NOT_PUT_BACK, OpenGuard, SAVING_CANCELLED, Problem, ProjectInfo, Saved, Store, Totals, epoch};
