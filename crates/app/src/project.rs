//! Saved projects: the recording and edit state of an uploaded tour, kept on this computer so the
//! tour can be opened and updated later. No UI lives here.
//!
//! [`state`] is the on-disk format (`project.json`) and its conversions to and from the editor's types.

// Nothing outside the tests uses the format until the project store is built on it.
#![allow(dead_code, unused_imports)]

mod state;

pub use state::{Blob, FORMAT, ParseError, ProjectFile, ProjectKey, Restored, SavedEdit, SavedEvent, SavedItem, Snapshot, from_file, parse, to_file, to_text};
