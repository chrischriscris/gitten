//! Which style each piece of a line is drawn in.
//!
//! Two independent sets of byte ranges describe every line of a diff, and they
//! do not nest: syntax tokens style the *foreground*, and the intraline spans
//! from [`intraline`](crate::intraline) light the *background* of the words that
//! actually changed. A renderer needs them as one flat, sorted, non-overlapping
//! list — the sweep that produces it is the same in every frontend, so it is
//! here rather than in three of them.
//!
//! It was written three times before it was written once. `plait-shell` merged
//! them into `HighlightStyle`s, `plait-web` merged them into text pieces for the
//! wire, and `core/examples/paint.rs` approximated the whole thing by
//! underlining anything a span touched. Two of those had the same off-by-one
//! available to them and one of them had it.
//!
//! # What a frontend still owns
//!
//! The style *type*. This yields a [`Surface`] and an optional [`Kind`] per run;
//! turning that into a `HighlightStyle`, an SGR escape or a JSON field is one
//! line each and is the only part that knows what draws it.
//!
//! # Coordinates
//!
//! Tokens and spans belong to the *line*, not to one of the rows it wraps onto,
//! so they stay in line coordinates throughout and are clamped into `at` on the
//! way in. [`Run::at`] comes back in line coordinates too — a caller slicing the
//! line indexes it directly, and one that wants row-relative offsets subtracts
//! `at.start`, which is cheaper than the alternative of clipping both inputs
//! into per-row vectors first.

use crate::syntax::{Kind, Token};
use crate::theme::Surface;
use crate::{LineKind, Span};
use std::ops::Range;

/// One stretch of a line that shares a foreground class and a background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Bytes of the *line* this run covers. Never empty.
    pub at: Range<usize>,
    /// Syntax class, or `None` for text no highlighter claimed.
    pub kind: Option<Kind>,
    /// Which background it lands on, which is what
    /// [`Theme::syntax_on`](crate::theme::Theme::syntax_on) needs to hand back a
    /// foreground that reads against it.
    pub surface: Surface,
    /// Inside an intraline span: the word that actually changed.
    ///
    /// Recoverable from `surface` for an added or removed line and *not* for a
    /// moved one, whose two halves have a single surface each — so it is its own
    /// field rather than a comparison a caller gets subtly wrong.
    pub word: bool,
}

/// The surfaces a line of this kind draws on: the row itself, and the changed
/// words inside it.
///
/// A moved block gets one surface for both, because nothing inside it changed —
/// see the note in [`runs`].
pub fn surfaces(kind: LineKind, moved: bool) -> (Surface, Surface) {
    match (kind, moved) {
        (LineKind::Added, false) => (Surface::Added, Surface::AddedWord),
        (LineKind::Added, true) => (Surface::MovedAdded, Surface::MovedAdded),
        (LineKind::Removed, false) => (Surface::Removed, Surface::RemovedWord),
        (LineKind::Removed, true) => (Surface::MovedRemoved, Surface::MovedRemoved),
        // Context is never moved: a line that did not change did not go
        // anywhere, and `differ::mark_moved` says so.
        (LineKind::Context, _) => (Surface::Context, Surface::Context),
    }
}

/// Merges `tokens` and `spans` over the bytes `at` into one flat run list.
///
/// **Gapless.** Every byte of `at` lands in exactly one run, in order, including
/// the stretches nothing claimed — a caller concatenating the pieces back into a
/// row would otherwise lose text rather than lose styling, and one that draws
/// backgrounds needs a run to paint the gap with.
///
/// Both inputs arrive sorted and internally non-overlapping from
/// [`prepared`](crate::prepared), so this is a sweep over their combined edges
/// rather than a sort of the pair.
///
/// `out` is cleared and reused: this runs once per visible row per frame, and
/// allocating a vector there is the one thing on the render path that must not
/// happen.
///
/// A **moved** line drops its spans. It is the same text in a different place,
/// so nothing inside it changed and its spans describe a change move detection
/// has just said was not one. Dropped here, once, rather than in each frontend:
/// an invisible run is still a run to merge and shape.
pub fn runs(
    at: Range<usize>,
    tokens: &[Token],
    spans: &[Span],
    kind: LineKind,
    moved: bool,
    out: &mut Vec<Run>,
) {
    out.clear();
    if at.start >= at.end {
        return;
    }
    let (plain, word_surface) = surfaces(kind, moved);
    let spans: &[Span] = if moved { &[] } else { spans };

    if tokens.is_empty() && spans.is_empty() {
        out.push(Run { at, kind: None, surface: plain, word: false });
        return;
    }

    // Clamped rather than filtered: anything wholly outside this row collapses
    // to a zero-length edge pair, which `dedup` removes for free. The ends of
    // `at` are seeded so the first run cannot start late and the last cannot
    // stop early — the gapless guarantee is this one line.
    let clamp = |i: usize| i.clamp(at.start, at.end);
    let mut edges = Vec::with_capacity((tokens.len() + spans.len()) * 2 + 2);
    edges.push(at.start);
    for t in tokens {
        edges.push(clamp(t.start));
        edges.push(clamp(t.end));
    }
    for s in spans {
        edges.push(clamp(s.start));
        edges.push(clamp(s.end));
    }
    edges.push(at.end);
    edges.sort_unstable();
    edges.dedup();

    let (mut ti, mut si) = (0usize, 0usize);
    let mut cursor = edges[0];
    for &edge in &edges[1..] {
        while ti < tokens.len() && tokens[ti].end <= cursor {
            ti += 1;
        }
        while si < spans.len() && spans[si].end <= cursor {
            si += 1;
        }
        let word = spans.get(si).is_some_and(|s| s.start <= cursor);
        out.push(Run {
            at: cursor..edge,
            kind: tokens.get(ti).filter(|t| t.start <= cursor).map(|t| t.kind),
            surface: if word { word_surface } else { plain },
            word,
        });
        cursor = edge;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Host;
    use crate::parse_unified_diff;
    use crate::prepared::prepare;

    fn tok(start: usize, end: usize, kind: Kind) -> Token {
        Token { start, end, kind }
    }

    fn span(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    /// Every invariant the render path depends on, in one place: sorted,
    /// touching, non-empty, and covering exactly the range asked for.
    fn well_formed(at: Range<usize>, out: &[Run]) {
        if at.start >= at.end {
            assert!(out.is_empty(), "an empty range produced {out:?}");
            return;
        }
        assert_eq!(out.first().unwrap().at.start, at.start, "starts late: {out:?}");
        assert_eq!(out.last().unwrap().at.end, at.end, "stops early: {out:?}");
        for r in out {
            assert!(r.at.start < r.at.end, "empty run in {out:?}");
        }
        for w in out.windows(2) {
            assert_eq!(w[0].at.end, w[1].at.start, "gap or overlap in {out:?}");
        }
    }

    #[test]
    fn unclaimed_text_is_one_run_and_not_no_runs() {
        // The difference between this and the shell's version it replaces: a
        // caller concatenating pieces needs the gap, and a caller painting
        // backgrounds needs something to paint it with.
        let mut out = Vec::new();
        runs(0..10, &[], &[], LineKind::Context, false, &mut out);
        well_formed(0..10, &out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, None);
        assert_eq!(out[0].surface, Surface::Context);
    }

    #[test]
    fn a_token_and_a_span_that_partly_overlap_split_into_three() {
        let mut out = Vec::new();
        runs(0..12, &[tok(0, 8, Kind::Str)], &[span(4, 12)], LineKind::Added, false, &mut out);
        well_formed(0..12, &out);
        let got: Vec<_> = out.iter().map(|r| (r.at.clone(), r.kind, r.surface, r.word)).collect();
        assert_eq!(
            got,
            vec![
                (0..4, Some(Kind::Str), Surface::Added, false),
                (4..8, Some(Kind::Str), Surface::AddedWord, true),
                (8..12, None, Surface::AddedWord, true),
            ]
        );
    }

    #[test]
    fn a_row_of_a_wrapped_line_clips_both_inputs_into_itself() {
        // Tokens and spans address the line; this row is the middle of it. The
        // failure this catches is a run pointing at a byte the row never draws.
        let mut out = Vec::new();
        let tokens = [tok(0, 4, Kind::Keyword), tok(10, 20, Kind::Str)];
        let spans = [span(2, 30)];
        runs(8..16, &tokens, &spans, LineKind::Removed, false, &mut out);
        well_formed(8..16, &out);
        assert!(out.iter().all(|r| r.at.start >= 8 && r.at.end <= 16), "{out:?}");
        // The keyword ended before this row; the string starts inside it.
        assert_eq!(out[0].kind, None);
        assert!(out.iter().any(|r| r.kind == Some(Kind::Str)));
        assert!(out.iter().all(|r| r.word), "the span covers the whole row");
    }

    #[test]
    fn a_moved_line_has_one_surface_and_no_changed_words() {
        let mut out = Vec::new();
        runs(0..6, &[], &[span(1, 3)], LineKind::Added, true, &mut out);
        well_formed(0..6, &out);
        assert_eq!(out.len(), 1, "the dropped span left an edge behind: {out:?}");
        assert_eq!(out[0].surface, Surface::MovedAdded);
        assert!(!out[0].word);
    }

    #[test]
    fn an_empty_range_produces_nothing_rather_than_a_panic() {
        let mut out = vec![Run { at: 0..1, kind: None, surface: Surface::Context, word: false }];
        runs(5..5, &[tok(0, 9, Kind::Str)], &[span(0, 9)], LineKind::Context, false, &mut out);
        assert!(out.is_empty(), "the buffer was not cleared");
    }

    #[test]
    fn every_run_of_a_real_diff_indexes_the_line_it_belongs_to() {
        // The whole pipeline, then the sweep over its output: nothing may point
        // past the text, and the pieces must reassemble it.
        let raw = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,3 @@
 fn one() {}
-let x = 1; // was
+let y = 1; // was
 fn two() {}
";
        let host = Host::new();
        let p = prepare(&parse_unified_diff(raw), &host.syntax, 2000);
        let mut out = Vec::new();
        let mut words = Vec::new();
        for l in p.files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines) {
            let at = 0..l.text.len();
            runs(at.clone(), &l.tokens, &l.spans, l.kind, l.moved, &mut out);
            well_formed(at, &out);
            let joined: String = out.iter().map(|r| &l.text[r.at.clone()]).collect();
            assert_eq!(joined, l.text, "the runs did not reassemble the line");
            words.extend(out.iter().filter(|r| r.word).map(|r| l.text[r.at.clone()].to_string()));
        }
        // `let x` -> `let y`: the identifier is the change, on both sides, and
        // the trailing comment is not.
        assert_eq!(words, vec!["x", "y"]);
    }
}
