//! gitten in the terminal it was started from.
//!
//! The third door, and the one that has to earn its keep by needing no logic of
//! its own. Acquisition is `gitten-git`, the differ and every pass over a diff is
//! `gitten-core`, and what is here is a grid of cells, the presentations that
//! fill it and the escape codes that put it on screen — the same division
//! `gitten-shell` has with GPUI and `gitten-web` has with a browser.
//!
//! ```text
//!   gitten-git ──► core::rows::assemble ──► Ordered ──► Rows::render ──► Screen
//!   two texts     prepare, claim,          8 bytes    one row of        cells,
//!   per file      order                    a row      cells             then ANSI
//! ```
//!
//! # What is different about a terminal, and what is not
//!
//! **Not** different: the pipeline, the seams and the row index space. A
//! [`rows::Rows`] implementation is claimed per path and cycles in a
//! [`rows::Layouts`] registry, exactly as in the shell; the order table is
//! `core`'s; a wrapped line is more rows rather than a taller one, for the same
//! reason it is there. A reading position means the same thing in all three
//! frontends because all three number their rows the same way.
//!
//! Different, and only these:
//!
//! - **A column is a cell, not a fraction of an em.** `Font::advance` has no
//!   answer here and `unicode-width` does, so [`screen::cols`] replaces it and
//!   `Rows::reflow` takes columns where the shell's takes pixels.
//! - **A view knows its own size before it draws.** GPUI hands a view its box
//!   during paint, which is why the shell's wrapping lands a frame late; a
//!   terminal is queried, so the budget is known up front.
//! - **The screen is ours.** [`screen::Screen`] is a cell buffer we own, which
//!   is what makes every view in here testable with no terminal at all — the one
//!   stage `docs/architecture.md` lists as untested in the shell.
//!
//! # Modules
//!
//! | | |
//! |---|---|
//! | [`screen`] | cells, ink, the pen, and the diff that becomes escape codes |
//! | [`rows`] | the `Rows` seam, `Layouts`, and `TextRows` |
//! | [`markdown`] | `MarkdownRows`: the rendered document, in cells |
//! | [`split`] | `SplitRows`: the two-column presentation |
//! | [`diff`] | the diff view: viewport, reflow, horizontal scroll |
//! | [`commits`] | the commit list, and the graph gutter in box drawing |
//! | [`files`] | the working tree: sections, files, and the armed discard |
//! | [`scrollbar`] | where you are in a list, drawn over its right-hand column |
//! | [`help`] | what the keys do, as a function of the keymap |
//! | [`term`] | the only module that touches `crossterm` |

pub mod commits;
pub mod diff;
pub mod files;
pub mod help;
pub mod markdown;
pub mod rows;
pub mod screen;
pub mod scrollbar;
pub mod split;
pub mod term;

/// The two rendering budgets, from `gitten_app` — where they are shared rather
/// than picked independently by three clients that all picked the same numbers.
pub use gitten_app::{MAX_LINE_CHARS, MIN_WRAP_COLS};
