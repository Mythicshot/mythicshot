//! Saved projects: the recording and edit state of an uploaded tour, kept on this computer so the
//! tour can be opened and updated later. No UI lives here.
//!
//! [`state`] is the on-disk format (`project.json`) and its conversions to and from the editor's types.
//! [`store`] is the folder the projects live in: save, list, open, remove.

// Nothing outside the tests uses the store until publishing keeps a recording and the Tours page opens one.
#![allow(dead_code, unused_imports)]

mod state;
mod store;

pub use state::{Blob, FORMAT, ItemEntry, MAX_PROJECT_JSON_BYTES, ParseError, ProjectFile, ProjectKey, Restored, SavedEdit, SavedEvent, SavedItem, Snapshot, from_file, parse, to_file, to_text};
pub use store::{LoadError, Loaded, NOT_PUT_BACK, OpenGuard, Problem, ProjectInfo, Saved, Store, Totals, epoch};
