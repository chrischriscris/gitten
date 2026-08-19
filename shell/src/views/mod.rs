//! Each view is a self-contained entity that fills whatever box it is handed.
//! None of them assume they own the window or the keymap — that is what makes
//! assembling the final multi-pane layout an assembly job rather than a rewrite.

pub mod commits;
pub mod diff;
