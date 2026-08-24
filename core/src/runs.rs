//! Which style each piece of a line is drawn in.
//!
//! Two independent sets of byte ranges describe every line of a diff, and they
//! do not nest: syntax tokens style the *foreground*, and the intraline spans
//! from [`intraline`](crate::intraline) light the *background* of the words that
//! actually changed. A renderer needs them as one flat, sorted, non-overlapping
//! list — the sweep that produces it is the same in every frontend, so it is
//! here rather than in three of them.
//!
//! It was written three times before it was written once. `gitten-shell` merged
//! them into `HighlightStyle`s, `gitten-web` merged them into text pieces for the
//! wire, and `core/examples/paint.rs` approximated the whole thing by
//! underlining anything a span touched. Two of those had the same off-by-one
//! available to them and one of them had it.
//!
//! # What a frontend still owns
//!
//! The style *type*. This yields a [`Surface`] and an optional [`Kind`] per run;
//! turning that into a `HighlightStyle`, an SGR escape or a JSON field is one
//! line each and is the only part that knows what draws it. A selection is a
//! [`Surface`] like the rest — [`runs_selected`] folds it in — so ranking it
//! against a changed word is decided once, here, and not per frontend.
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
/// **Allocation-free after the first call.** Both inputs arrive sorted and
/// internally non-overlapping from [`prepared`](crate::prepared), so this walks
/// them together and emits each run as it finds it. Collecting the combined
/// edges into a vector first is the obvious way to write it, it is what this
/// replaced, and it is a `Vec` per visible row per frame for an answer the sweep
/// already had. `out` is cleared and reused, which is the one thing on a render
/// path that must not allocate.
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
    runs_selected(at, tokens, spans, kind, moved, 0..0, out);
}

/// [`runs`], with a **selection** folded in: `sel` is the part of the line the
/// mouse is holding, in line coordinates like everything else.
///
/// A selection outranks a changed word — both are backgrounds and only one can
/// be drawn, and the reader already knows which words changed; what they are
/// about to press a key about is what is selected. Runs split at its
/// boundaries and every byte inside it comes back on [`Surface::Selected`], so
/// a caller resolving foregrounds against the surface sees the background that
/// will actually be painted. Empty — nearly every row of nearly every frame —
/// it changes nothing, which is what makes `runs` above a plain delegation.
pub fn runs_selected(
    at: Range<usize>,
    tokens: &[Token],
    spans: &[Span],
    kind: LineKind,
    moved: bool,
    sel: Range<usize>,
    out: &mut Vec<Run>,
) {
    out.clear();
    if at.start >= at.end {
        return;
    }
    // Clamped into `at` like tokens and spans — a drag across several rows
    // reaches each one as the part of itself it covers.
    let sel = sel.start.clamp(at.start, at.end)..sel.end.clamp(at.start, at.end);
    let selecting = |c: usize| sel.start < sel.end && sel.start <= c && c < sel.end;
    let (plain, word_surface) = surfaces(kind, moved);
    let spans: &[Span] = if moved { &[] } else { spans };

    // Nothing claimed any byte of this row: one plain run and done. A
    // non-empty selection always reaches this row once clamped, so it
    // disqualifies the fast path — part of the row is somebody else's
    // background even when nothing else is.
    if tokens.is_empty() && spans.is_empty() && sel.start >= sel.end {
        out.push(Run {
            at,
            kind: None,
            surface: plain,
            word: false,
        });
        return;
    }

    let (mut ti, mut si) = (0usize, 0usize);
    let mut cursor = at.start;
    while cursor < at.end {
        // Anything ending at or before the cursor is behind us. Not clamped into
        // `at`: a token that runs past the end of this row is still the token
        // covering the cursor, and clamping it here would skip it.
        while ti < tokens.len() && tokens[ti].end as usize <= cursor {
            ti += 1;
        }
        while si < spans.len() && spans[si].end as usize <= cursor {
            si += 1;
        }
        // Whichever of the two actually covers the cursor, if either does.
        let tok = tokens.get(ti).filter(|t| t.start as usize <= cursor);
        let spn = spans.get(si).filter(|s| s.start as usize <= cursor);

        // The next byte at which any of that changes: where a live range ends,
        // or where a pending one begins. Every candidate is strictly past the
        // cursor — a live range's end is, because it was not skipped, and a
        // pending one's start is, because it is not live — so the run advances
        // and this terminates. The selection's edges are candidates by the same
        // argument: inside it, its end; before it, its start.
        let mut edge = at.end;
        match tok {
            Some(t) => edge = edge.min(t.end as usize),
            None => {
                if let Some(t) = tokens.get(ti) {
                    edge = edge.min(t.start as usize);
                }
            }
        }
        match spn {
            Some(s) => edge = edge.min(s.end as usize),
            None => {
                if let Some(s) = spans.get(si) {
                    edge = edge.min(s.start as usize);
                }
            }
        }
        if sel.start < sel.end {
            if selecting(cursor) {
                edge = edge.min(sel.end);
            } else if sel.start > cursor {
                edge = edge.min(sel.start);
            }
        }

        let word = spn.is_some();
        out.push(Run {
            at: cursor..edge,
            kind: tok.map(|t| t.kind),
            surface: if selecting(cursor) {
                Surface::Selected
            } else if word {
                word_surface
            } else {
                plain
            },
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

    fn tok(start: u32, end: u32, kind: Kind) -> Token {
        Token { start, end, kind }
    }

    fn span(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    /// Every invariant the render path depends on, in one place: sorted,
    /// touching, non-empty, and covering exactly the range asked for.
    fn well_formed(at: Range<usize>, out: &[Run]) {
        if at.start >= at.end {
            assert!(out.is_empty(), "an empty range produced {out:?}");
            return;
        }
        assert_eq!(
            out.first().unwrap().at.start,
            at.start,
            "starts late: {out:?}"
        );
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
        runs(
            0..12,
            &[tok(0, 8, Kind::Str)],
            &[span(4, 12)],
            LineKind::Added,
            false,
            &mut out,
        );
        well_formed(0..12, &out);
        let got: Vec<_> = out
            .iter()
            .map(|r| (r.at.clone(), r.kind, r.surface, r.word))
            .collect();
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
        assert!(
            out.iter().all(|r| r.at.start >= 8 && r.at.end <= 16),
            "{out:?}"
        );
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
        assert_eq!(
            out.len(),
            1,
            "the dropped span left an edge behind: {out:?}"
        );
        assert_eq!(out[0].surface, Surface::MovedAdded);
        assert!(!out[0].word);
    }

    #[test]
    fn a_selection_splits_the_runs_at_its_own_edges() {
        // No tokens, no spans, one selection in the middle: the plain stretches
        // either side of it are still runs, and the selected bytes are on the
        // selection surface even though nothing else claimed them.
        let mut out = Vec::new();
        runs_selected(0..10, &[], &[], LineKind::Context, false, 3..7, &mut out);
        well_formed(0..10, &out);
        let got: Vec<_> = out.iter().map(|r| (r.at.clone(), r.surface)).collect();
        assert_eq!(
            got,
            vec![
                (0..3, Surface::Context),
                (3..7, Surface::Selected),
                (7..10, Surface::Context),
            ]
        );
    }

    #[test]
    fn a_selection_outranks_a_changed_word_and_resolves_against_its_background() {
        let mut plain = Vec::new();
        let mut selected = Vec::new();
        runs_selected(
            0..9,
            &[tok(2, 7, Kind::Str)],
            &[span(4, 8)],
            LineKind::Added,
            false,
            5..9,
            &mut selected,
        );
        runs_selected(
            0..9,
            &[tok(2, 7, Kind::Str)],
            &[span(4, 8)],
            LineKind::Added,
            false,
            0..0,
            &mut plain,
        );
        well_formed(0..9, &selected);
        // The word's bytes 5..8 were Selected; only 4..5 kept the word surface,
        // the token underneath split the selected stretch at its own edge, and
        // the unclaimed tail past the word is selected too — the selection runs
        // to the end of the row.
        assert_eq!(plain[2].surface, Surface::AddedWord);
        let picked: Vec<_> = selected
            .iter()
            .filter(|r| r.surface == Surface::Selected)
            .map(|r| r.at.clone())
            .collect();
        assert_eq!(picked, vec![5..7, 7..8, 8..9]);
        assert_eq!(selected[2].at, 4..5);
        assert_eq!(selected[2].surface, Surface::AddedWord);
    }

    #[test]
    fn an_empty_selection_is_exactly_the_plain_sweep() {
        let tokens = [tok(1, 4, Kind::Keyword), tok(6, 9, Kind::Str)];
        let spans = [span(2, 8)];
        let mut a = Vec::new();
        let mut b = Vec::new();
        runs(0..10, &tokens, &spans, LineKind::Removed, false, &mut a);
        runs_selected(
            0..10,
            &tokens,
            &spans,
            LineKind::Removed,
            false,
            3..3,
            &mut b,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn an_empty_range_produces_nothing_rather_than_a_panic() {
        let mut out = vec![Run {
            at: 0..1,
            kind: None,
            surface: Surface::Context,
            word: false,
        }];
        runs(
            5..5,
            &[tok(0, 9, Kind::Str)],
            &[span(0, 9)],
            LineKind::Context,
            false,
            &mut out,
        );
        assert!(out.is_empty(), "the buffer was not cleared");
    }

    #[test]
    fn a_reused_buffer_stops_allocating() {
        // The property the sweep exists for: 50 visible rows repainted on every
        // keystroke, and no `Vec` grown after the first frame.
        let tokens: Vec<Token> = (0..20).map(|i| tok(i * 4, i * 4 + 3, Kind::Str)).collect();
        let spans: Vec<Span> = (0..10).map(|i| span(i * 8 + 1, i * 8 + 5)).collect();
        let mut out = Vec::new();
        runs(0..80, &tokens, &spans, LineKind::Added, false, &mut out);
        well_formed(0..80, &out);
        let capacity = out.capacity();
        for _ in 0..100 {
            runs(0..80, &tokens, &spans, LineKind::Added, false, &mut out);
        }
        assert_eq!(out.capacity(), capacity, "the buffer grew on a repaint");
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
            assert_eq!(joined, &*l.text, "the runs did not reassemble the line");
            words.extend(
                out.iter()
                    .filter(|r| r.word)
                    .map(|r| l.text[r.at.clone()].to_string()),
            );
        }
        // `let x` -> `let y`: the identifier is the change, on both sides, and
        // the trailing comment is not.
        assert_eq!(words, vec!["x", "y"]);
    }
}
