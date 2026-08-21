//! Which removed line sits beside which added one.
//!
//! A unified diff prints a hunk as one column: removals, then the additions
//! that replaced them. A side-by-side one has to put a pair on the same row, and
//! that is a decision — three lines deleted and five added is not five rows or
//! three, and which of the five sits opposite which of the three is a guess
//! somebody has to make.
//!
//! # Why this is not in the renderer
//!
//! Because [`crate::replace_pairs`] already makes exactly this guess, for the
//! intraline pass, and the two answers have to agree. If they do not, a row
//! shows a removal beside an addition whose changed words were computed against
//! a *different* line — highlighted fragments that do not correspond to anything
//! on screen. So there is one function, and both callers use it.
//!
//! [`align`] is therefore the primitive and [`pairs`] a filter over it, rather
//! than two scans of the same rule. `align` is the pairs plus everything around
//! them, in reading order; `pairs` is just the pairs.
//!
//! # The rule
//!
//! A run of N removals immediately followed by M additions pairs index-wise.
//! `min(N, M)` rows carry both sides; the leftovers stand alone. It is what a
//! human reading a hunk assumes, it is what every side-by-side viewer does, and
//! it is right whenever someone edited a block of lines in place — which is most
//! of the time. When it is wrong the pair simply does not resemble itself, and
//! `MIN_INTRALINE_SIMILARITY` is what stops that being highlighted as a rewrite.

use crate::LineKind;

/// One row of a two-column diff, as indices into the hunk's line list.
///
/// Indices rather than references so this can be computed once and held beside
/// rows the renderer owns, and `u32` so a slot is 12 bytes — at 700k rows the
/// difference between this and anything boxed is the whole scroll budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Unchanged: the same text belongs in both columns.
    Context(u32),
    /// A removal and the addition that replaced it, side by side.
    Replace(u32, u32),
    /// A removal with nothing opposite it.
    Removed(u32),
    /// An addition with nothing opposite it.
    Added(u32),
}

impl Slot {
    /// The line to draw in the left column, if any.
    pub fn old(self) -> Option<u32> {
        match self {
            Slot::Context(i) | Slot::Replace(i, _) | Slot::Removed(i) => Some(i),
            Slot::Added(_) => None,
        }
    }

    /// The line to draw in the right column, if any.
    pub fn new(self) -> Option<u32> {
        match self {
            Slot::Context(i) | Slot::Replace(_, i) | Slot::Added(i) => Some(i),
            Slot::Removed(_) => None,
        }
    }
}

/// One row per slot, in reading order, for a hunk with these line kinds.
///
/// Takes kinds rather than lines because that is all it needs, which makes it
/// callable from a renderer holding already-prepared rows without handing it the
/// text back.
pub fn align(kinds: &[LineKind]) -> Vec<Slot> {
    let mut out = Vec::with_capacity(kinds.len());
    let mut i = 0;
    while i < kinds.len() {
        match kinds[i] {
            LineKind::Context => {
                out.push(Slot::Context(i as u32));
                i += 1;
            }
            // An addition run with no removals before it. Still a run, with an
            // empty removal side, so a pure insertion goes down the same path —
            // a scan that only opens a run on a removal drops it entirely.
            LineKind::Added => {
                while i < kinds.len() && kinds[i] == LineKind::Added {
                    out.push(Slot::Added(i as u32));
                    i += 1;
                }
            }
            LineKind::Removed => {
                let del_start = i;
                while i < kinds.len() && kinds[i] == LineKind::Removed {
                    i += 1;
                }
                let dels = del_start..i;
                while i < kinds.len() && kinds[i] == LineKind::Added {
                    i += 1;
                }
                let adds = dels.end..i;
                let both = dels.len().min(adds.len());
                for k in 0..both {
                    out.push(Slot::Replace((dels.start + k) as u32, (adds.start + k) as u32));
                }
                for j in dels.start + both..dels.end {
                    out.push(Slot::Removed(j as u32));
                }
                for j in adds.start + both..adds.end {
                    out.push(Slot::Added(j as u32));
                }
            }
        }
    }
    out
}

/// The pairs and only the pairs, for the intraline pass.
///
/// Derived from [`align`] rather than scanned again: two scans of the same rule
/// is two places for it to drift, and the transient `Vec<Slot>` is one hunk's
/// worth of 12-byte values against a pass that is already allocating a `String`
/// per line.
pub fn pairs(kinds: &[LineKind]) -> Vec<(usize, usize)> {
    align(kinds)
        .into_iter()
        .filter_map(|s| match s {
            Slot::Replace(o, n) => Some((o as usize, n as usize)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_unified_diff, replace_pairs};
    // `LineKind` and `Slot` share three variant names, which is the right
    // naming on both and unusable as two globs.
    use crate::LineKind::{Added as A, Context as C, Removed as R};
    use Slot::*;

    #[test]
    fn context_is_one_row_with_both_columns() {
        assert_eq!(align(&[C, C]), vec![Context(0), Context(1)]);
    }

    #[test]
    fn a_replaced_block_pairs_index_wise() {
        let kinds = [C, R, R, A, A, C];
        assert_eq!(
            align(&kinds),
            vec![Context(0), Replace(1, 3), Replace(2, 4), Context(5)]
        );
    }

    #[test]
    fn the_longer_side_of_an_uneven_run_stands_alone() {
        // Two lines became four: two rows show a pair, two show only the new
        // side. The alternative — spreading two removals over four rows — puts
        // blank space between lines that belong together.
        let kinds = [R, R, A, A, A, A];
        assert_eq!(
            align(&kinds),
            vec![Replace(0, 2), Replace(1, 3), Added(4), Added(5)]
        );

        let kinds = [R, R, R, A];
        assert_eq!(align(&kinds), vec![Replace(0, 3), Removed(1), Removed(2)]);
    }

    #[test]
    fn a_pure_insertion_or_deletion_has_one_empty_column() {
        assert_eq!(align(&[A, A]), vec![Added(0), Added(1)]);
        assert_eq!(align(&[R, R]), vec![Removed(0), Removed(1)]);
    }

    #[test]
    fn an_addition_run_before_any_removal_is_still_a_run() {
        // The ordering trap: additions can come first in a hunk, and a scan that
        // only opens a run on a removal drops them.
        assert_eq!(align(&[A, R]), vec![Added(0), Removed(1)]);
    }

    #[test]
    fn every_line_appears_exactly_once() {
        // The invariant a two-column view depends on: no line drawn twice, none
        // dropped. Over every shape of run.
        let shapes: [&[LineKind]; 7] = [
            &[],
            &[C],
            &[R, A],
            &[A, R, C, R, A, A],
            &[R, R, R],
            &[A, C, A],
            &[C, R, A, R, A, C, A],
        ];
        for kinds in shapes {
            let slots = align(kinds);
            let mut seen = vec![0u32; kinds.len()];
            for s in &slots {
                for i in s.old().into_iter().chain(s.new()) {
                    seen[i as usize] += 1;
                }
            }
            for (i, n) in seen.iter().enumerate() {
                let expected = if kinds[i] == C { 2 } else { 1 };
                assert_eq!(*n, expected, "line {i} of {kinds:?} appeared {n} times");
            }
        }
    }

    #[test]
    fn alignment_and_the_intraline_pairing_are_the_same_answer() {
        // The whole reason this lives in core next to `replace_pairs`. A row
        // pairing a removal with an addition whose changed words were computed
        // against a different line shows highlighted fragments that correspond
        // to nothing.
        let raw = "\
diff --git a/x b/x
@@ -1,6 +1,7 @@
 keep
-old one
-old two
-old three
+new one
+new two
 tail
+appended
";
        let hunk = &parse_unified_diff(raw)[0].hunks[0];
        let kinds: Vec<LineKind> = hunk.lines.iter().map(|l| l.kind).collect();
        let from_align: Vec<(usize, usize)> = align(&kinds)
            .into_iter()
            .filter_map(|s| match s {
                Replace(o, n) => Some((o as usize, n as usize)),
                _ => None,
            })
            .collect();
        assert_eq!(from_align, replace_pairs(hunk));
        assert_eq!(from_align, vec![(1, 4), (2, 5)]);
    }
}
