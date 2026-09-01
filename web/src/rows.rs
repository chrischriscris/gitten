//! The diff, flattened into the rows a list scrolls.
//!
//! Nearly nothing, and deliberately: the flattening, the break table, the
//! reflow and the mapping from a visual row back to a line are all
//! [`gitten_core::rows`], because a browser needing the same row *index space*
//! as the window is the whole reason that module exists. What is left here is
//! the pair of durations a stats readout wants and a name to hold it all under.
//!
//! The one thing this door has an opinion about is which of `core`'s two
//! mappings it holds — [`Visual`], the prefix sum, rather than the order table
//! the desktop list iterates. A request names a window out of nowhere and pays
//! one binary search to find its start; see [`gitten_core::rows::Visual`] for
//! why that is the seam rather than a second implementation.
//!
//! What the browser is left with is drawing, which is the whole point of
//! [`gitten_core::prepared`] — see its module docs.

use gitten_core::prepared::Prepared;
use gitten_core::rows::{Entry, Flat, Row, Visual};
use gitten_core::wrap::Wrap;
use std::time::Duration;

pub struct Doc {
    flat: Flat,
    index: Visual,
    pub intraline: Duration,
    pub syntax: Duration,
}

impl Doc {
    pub fn build(p: Prepared) -> Self {
        let mut flat = Flat::default();
        let (intraline, syntax) = (p.intraline, p.syntax);
        for f in p.files {
            flat.push(f);
        }
        let index = Visual::build(&flat);
        Self {
            flat,
            index,
            intraline,
            syntax,
        }
    }

    pub fn rows(&self) -> &[Row] {
        self.flat.rows()
    }

    pub fn files(&self) -> &[Entry] {
        self.flat.files()
    }

    /// Rows that are part of a block that moved. Reported for the same reason
    /// the desktop overlay reports it: move detection finding nothing and move
    /// detection being switched off look identical on screen.
    pub fn moved(&self) -> usize {
        self.flat.moved()
    }

    /// Rebuilds the break table for a new column budget, and says whether
    /// anything moved.
    ///
    /// The budget arrives from the client, because how wide a row may get is a
    /// property of a window and `core` cannot know it. Re-indexing is skipped
    /// along with the rest when nothing changed, which is what makes a drag that
    /// does not cross a character boundary free.
    pub fn reflow(&mut self, cols: usize, wrap: &dyn Wrap) -> bool {
        if !self.flat.reflow(cols, wrap) {
            return false;
        }
        self.index = Visual::build(&self.flat);
        true
    }

    /// Total visual rows — what the client sizes its scrollbar to.
    pub fn total(&self) -> usize {
        self.index.total()
    }

    pub fn cols(&self) -> usize {
        self.flat.cols()
    }

    pub fn wrap_name(&self) -> &'static str {
        self.flat.wrap_name()
    }

    /// Breaks a third-party [`Wrap`] produced that were thrown away. Surfaced
    /// rather than swallowed: a wrap quietly not working looks exactly like a
    /// wrap with nothing to do.
    pub fn rejected(&self) -> usize {
        self.flat.rejected()
    }

    /// The first visual row of a logical one.
    pub fn visual(&self, logical: usize) -> usize {
        self.index.first(logical)
    }

    /// Which logical row a visual row belongs to, and which of its rows it is.
    pub fn at(&self, visual: usize) -> Option<(usize, usize)> {
        self.index.at(visual)
    }

    /// The text visual row `seg` of logical row `logical` actually draws.
    pub fn piece(&self, logical: usize, seg: usize) -> &str {
        self.flat.piece(logical, seg)
    }

    /// The bytes of a row's text that segment `seg` draws — line coordinates,
    /// which is what [`gitten_core::runs`] wants.
    pub fn range(&self, logical: usize, seg: usize) -> std::ops::Range<usize> {
        self.flat.range(logical, seg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::host::Host;
    use gitten_core::parse_unified_diff;
    use gitten_core::prepared::prepare;
    use gitten_core::wrap::Word;

    const DIFF: &str = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,3 @@
 fn one() {}
-let x = 1;
+let x = 2;
 fn two() {}
";

    fn doc() -> Doc {
        let host = Host::new();
        Doc::build(prepare(&parse_unified_diff(DIFF), &host.syntax, 2000))
    }

    /// The wiring, and only the wiring: a reflow that moves the break table has
    /// to move the index built over it, or every window served afterwards
    /// addresses rows by the shape the diff had before the resize. `core` owns
    /// both halves and tests each; nothing there can catch them drifting apart
    /// here.
    #[test]
    fn a_reflow_rebuilds_the_index_over_the_breaks_it_made() {
        let mut d = doc();
        assert_eq!(d.total(), d.rows().len(), "unwrapped, one row each");

        assert!(d.reflow(8, &Word));
        assert!(d.total() > d.rows().len(), "nothing wrapped");
        assert_eq!(d.cols(), 8);
        assert_eq!(d.wrap_name(), "word");
        // Every visual row still names a row that exists, and the last one is
        // the end rather than something past it.
        assert!(d.at(d.total() - 1).is_some());
        assert_eq!(d.at(d.total()), None);

        assert!(!d.reflow(8, &Word), "the same budget is not a rebuild");
    }
}
