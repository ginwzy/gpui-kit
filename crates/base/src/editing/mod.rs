//! Editing mechanics shared by text-bearing behavior systems.
//!
//! This module stays crate-private until more than one public state proves a
//! smaller stable seam. Existing input facades consume each extracted
//! primitive immediately; this is not a parallel editor implementation.

mod selection;

pub(crate) use selection::{CursorSelection, Selections};
