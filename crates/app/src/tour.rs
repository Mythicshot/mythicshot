//! Tour export: turns a recording into a tour bundle, one image per click.
//!
//! A tour step is the frame from just before a left click plus where the
//! click landed. Step images are drawn by [`crate::video::decorate`] with the
//! overlays off, so annotations and crop match every other export.

// Nothing calls into this module until the editor's "Export tour…" button
// lands; remove this line then.
#![allow(dead_code)]

mod encode;

pub use encode::*;
