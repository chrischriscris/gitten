//! The diff, flattened into the rows a list scrolls.
//!
//! This is `TextRows` from the shell with the drawing taken out, and it is a
//! deliberate mirror rather than a fresh design: the same three row kinds in the
//! same order, the same `Wrapped` from the same registry, the same column check
//! on reflow. Keeping the row *index space* identical to the desktop's is what
//! makes a saved reading position, a row count and a bug report mean the same
//! thing in both frontends.
//!
//! What the browser is left with is drawing, which is the whole point of
//! `plait_core::prepared` — see its module docs.

use plait_core::prepared::{File as PreparedFile, Prepared};
use plait_core::syntax::{Kind, Token};
use plait_core::wrap::{Wrap, Wrapped};
use plait_core::{LineKind, Span};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

pub enum Row {
    File {
        path: String,
        adds: usize,
        dels: usize,
    },
    Hunk(String),
    Line(Line),
}

pub struct Line {
    pub kind: LineKind,
    pub moved: bool,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// Shared with the prepared line it came from — a move, not a copy.
    pub text: Arc<str>,
    pub spans: Box<[Span]>,
    pub tokens: Box<[Token]>,
}

/// A file's place in the flat row list, for the jump list in the sidebar.
pub struct Entry {
    pub path: String,
    pub adds: usize,
    pub dels: usize,
    /// Which *logical* row is this file's header. Turned into a visual row by
    /// [`Doc::visual`] at request time, because reflow moves it and this does
    /// not want invalidating every time the window is dragged.
    pub row: usize,
}

pub struct Doc {
    pub rows: Vec<Row>,
    pub files: Vec<Entry>,
    /// Rows that are part of a block that moved. Reported for the same reason
    /// the overlay reports it: move detection finding nothing and move detection
    /// being switched off look identical on screen.
    pub moved: usize,
    pub intraline: Duration,
    pub syntax: Duration,
    wrapped: Wrapped,
    cols: usize,
    wrap: &'static str,
    /// `starts[i]` is the first visual row of logical row `i`; one longer than
    /// `rows`, so the last row needs no special case.
    ///
    /// A prefix sum and a binary search rather than the shell's one-entry-per-
    /// visual-row order table: a request asks for a window of a few dozen rows
    /// and pays a `log n` to find its start, where the list has to *iterate* its
    /// order table and wants the flat one. 714k rows is 2.8 MB here either way,
    /// and this one does not need rebuilding to answer "how many rows are there".
    starts: Vec<u32>,
}

impl Doc {
    pub fn build(p: Prepared) -> Self {
        let mut doc = Self {
            rows: Vec::new(),
            files: Vec::new(),
            moved: 0,
            intraline: p.intraline,
            syntax: p.syntax,
            wrapped: Wrapped::build(std::iter::empty::<(&str, usize)>(), &plait_core::wrap::Off),
            cols: 0,
            wrap: "",
            starts: Vec::new(),
        };
        for f in p.files {
            doc.push_file(f);
        }
        doc.index();
        doc
    }

    fn push_file(&mut self, f: PreparedFile) {
        self.files.push(Entry {
            path: f.path.clone(),
            adds: f.adds,
            dels: f.dels,
            row: self.rows.len(),
        });
        self.rows.push(Row::File {
            path: f.path,
            adds: f.adds,
            dels: f.dels,
        });
        for h in f.hunks {
            self.rows.push(Row::Hunk(h.header));
            for l in h.lines {
                self.moved += l.moved as usize;
                self.rows.push(Row::Line(Line {
                    kind: l.kind,
                    moved: l.moved,
                    old_no: l.old_no,
                    new_no: l.new_no,
                    text: l.text,
                    spans: l.spans,
                    tokens: l.tokens,
                }));
            }
        }
    }

    /// The text of a row that may wrap. A header does not, and an empty string
    /// is how [`Wrapped`] is told so — the same trick the shell's `wrappable`
    /// plays, and for the same reason: a parallel table of which rows are lines
    /// is a second thing to keep in step.
    fn wrappable(row: &Row) -> &str {
        match row {
            Row::Line(l) => &l.text,
            _ => "",
        }
    }

    /// Rebuilds the break table for a new column budget, and says whether
    /// anything moved.
    ///
    /// The budget arrives from the client, because how wide a row may get is a
    /// property of a window and `core` cannot know it — the same contract
    /// `Rows::reflow` has with the shell and `WRAP_COLS` has with `paint`. The
    /// early return is what makes a drag that does not cross a character
    /// boundary free.
    pub fn reflow(&mut self, cols: usize, wrap: &dyn Wrap) -> bool {
        if cols == self.cols && wrap.name() == self.wrap {
            return false;
        }
        self.cols = cols;
        self.wrap = wrap.name();
        self.wrapped = Wrapped::build(self.rows.iter().map(|r| (Self::wrappable(r), cols)), wrap);
        self.index();
        true
    }

    fn index(&mut self) {
        self.starts.clear();
        self.starts.reserve(self.rows.len() + 1);
        let mut at = 0u32;
        for i in 0..self.rows.len() {
            self.starts.push(at);
            at += self.wrapped.rows(i) as u32;
        }
        self.starts.push(at);
    }

    /// Total visual rows — what the client sizes its scrollbar to.
    pub fn total(&self) -> usize {
        self.starts.last().copied().unwrap_or(0) as usize
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn wrap_name(&self) -> &'static str {
        self.wrap
    }

    /// Breaks a third-party [`Wrap`] produced that were thrown away. Surfaced
    /// rather than swallowed: a wrap quietly not working looks exactly like a
    /// wrap with nothing to do.
    pub fn rejected(&self) -> usize {
        self.wrapped.rejected()
    }

    /// The first visual row of a logical one.
    pub fn visual(&self, logical: usize) -> usize {
        self.starts.get(logical).copied().unwrap_or(0) as usize
    }

    /// Which logical row a visual row belongs to, and which of its rows it is.
    ///
    /// `partition_point` over the prefix sums. Out of range clamps to the last
    /// row rather than panicking: the client can ask for a window past the end
    /// while a reflow is in flight, and an empty answer is the honest reply.
    pub fn at(&self, visual: usize) -> Option<(usize, usize)> {
        if self.rows.is_empty() || visual >= self.total() {
            return None;
        }
        let v = visual as u32;
        let i = self.starts.partition_point(|&s| s <= v) - 1;
        Some((i, (v - self.starts[i]) as usize))
    }

    /// The bytes of a row's text that segment `seg` draws.
    pub fn range(&self, logical: usize, seg: usize, text: &str) -> Range<usize> {
        self.wrapped.range(logical, seg, text)
    }
}

/// One run of a row's text that shares a foreground style and a background.
///
/// The client is handed pieces of *text* and not byte offsets into a line, on
/// purpose. `Span` and `Token` both address bytes; a JavaScript string is
/// UTF-16, so handing them over means every consumer converts, and the one that
/// forgets breaks on exactly the lines a diff of anything non-English is made of.
/// Slicing here also means the browser does a `for` over pieces instead of a
/// sweep per row per frame.
pub struct Piece<'a> {
    pub text: &'a str,
    /// Syntax class, or `None` for text no highlighter claimed.
    pub kind: Option<Kind>,
    /// Inside an intraline span — the word that actually changed.
    pub word: bool,
}

/// Merges tokens and spans into the flat, sorted, gapless run list the client
/// draws.
///
/// This is the shell's `runs` with two differences, both forced by the wire.
/// Gaps are *emitted* rather than skipped, because the pieces are concatenated
/// back into the row and a skipped gap is missing text rather than unstyled
/// text. And `at.start`/`at.end` are seeded into the edge list, so the first and
/// last piece cannot start late or stop early.
///
/// Tokens and spans stay in *line* coordinates throughout and are clamped into
/// `at` on the way in — they belong to the line, not to one of its rows.
pub fn pieces<'a>(line: &'a Line, at: Range<usize>, out: &mut Vec<Piece<'a>>) {
    out.clear();
    // A moved line is the same text somewhere else, so nothing inside it
    // changed and its spans describe a change the detection just said was not
    // one. Dropped here exactly as the shell drops it.
    let spans: &[Span] = if line.moved { &[] } else { &line.spans };
    let tokens = &line.tokens;

    if at.start >= at.end {
        return;
    }
    if tokens.is_empty() && spans.is_empty() {
        out.push(Piece {
            text: &line.text[at],
            kind: None,
            word: false,
        });
        return;
    }

    let clamp = |i: usize| i.clamp(at.start, at.end);
    let mut edges = Vec::with_capacity((tokens.len() + spans.len()) * 2 + 2);
    edges.push(at.start);
    for t in tokens {
        edges.push(clamp(t.start as usize));
        edges.push(clamp(t.end as usize));
    }
    for s in spans {
        edges.push(clamp(s.start as usize));
        edges.push(clamp(s.end as usize));
    }
    edges.push(at.end);
    edges.sort_unstable();
    edges.dedup();

    let (mut ti, mut si) = (0usize, 0usize);
    let mut cursor = edges[0];
    for &edge in &edges[1..] {
        while ti < tokens.len() && tokens[ti].end as usize <= cursor {
            ti += 1;
        }
        while si < spans.len() && spans[si].end as usize <= cursor {
            si += 1;
        }
        let word = spans.get(si).is_some_and(|s| s.start as usize <= cursor);
        let kind = tokens
            .get(ti)
            .filter(|t| t.start as usize <= cursor)
            .map(|t| t.kind);
        out.push(Piece {
            text: &line.text[cursor..edge],
            kind,
            word,
        });
        cursor = edge;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plait_core::host::Host;
    use plait_core::parse_unified_diff;
    use plait_core::prepared::prepare;
    use plait_core::wrap::{Off, Word};

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

    #[test]
    fn a_file_becomes_a_header_a_hunk_header_and_a_row_per_line() {
        let d = doc();
        assert!(matches!(d.rows[0], Row::File { .. }));
        assert!(matches!(d.rows[1], Row::Hunk(_)));
        assert_eq!(d.rows.len(), 2 + 4);
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.files[0].row, 0);
    }

    #[test]
    fn unwrapped_every_row_is_one_row() {
        let mut d = doc();
        d.reflow(0, &Off);
        assert_eq!(d.total(), d.rows.len());
        assert_eq!(d.at(3), Some((3, 0)));
    }

    #[test]
    fn a_reflow_to_the_same_budget_reports_no_change() {
        let mut d = doc();
        assert!(d.reflow(20, &Word));
        assert!(!d.reflow(20, &Word));
        assert!(d.reflow(20, &Off));
    }

    #[test]
    fn wrapping_adds_rows_and_the_extra_rows_map_back_to_their_line() {
        let mut d = doc();
        d.reflow(6, &Word);
        assert!(d.total() > d.rows.len());
        // Every visual row resolves, and the segments of one line are
        // consecutive and start at zero.
        let mut seen = vec![Vec::new(); d.rows.len()];
        for v in 0..d.total() {
            let (i, seg) = d.at(v).expect("every visual row inside total resolves");
            seen[i].push(seg);
        }
        for segs in &seen {
            assert_eq!(*segs, (0..segs.len()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn a_row_past_the_end_is_none_rather_than_a_panic() {
        let mut d = doc();
        d.reflow(0, &Off);
        assert_eq!(d.at(d.total()), None);
        assert_eq!(d.at(usize::MAX), None);
    }

    #[test]
    fn the_pieces_of_a_row_concatenate_back_into_it() {
        let mut d = doc();
        d.reflow(6, &Word);
        let mut out = Vec::new();
        for v in 0..d.total() {
            let (i, seg) = d.at(v).unwrap();
            let Row::Line(l) = &d.rows[i] else { continue };
            let at = d.range(i, seg, &l.text);
            pieces(l, at.clone(), &mut out);
            let joined: String = out.iter().map(|p| p.text).collect();
            assert_eq!(joined, l.text[at], "row {v} lost or gained text");
        }
    }

    #[test]
    fn the_changed_word_is_the_only_piece_marked_as_one() {
        let d = doc();
        let mut out = Vec::new();
        let mut marked = Vec::new();
        for r in &d.rows {
            let Row::Line(l) = r else { continue };
            pieces(l, 0..l.text.len(), &mut out);
            for p in out.iter().filter(|p| p.word) {
                marked.push(p.text.to_string());
            }
        }
        // `let x = 1;` -> `let x = 2;`: the digit is the change, on both sides.
        assert_eq!(marked, vec!["1", "2"]);
    }

    #[test]
    fn a_moved_line_reports_no_changed_words() {
        let d = doc();
        let mut out = Vec::new();
        let Row::Line(l) = &d.rows[3] else {
            panic!("row 3 is a line")
        };
        assert!(
            !l.spans.is_empty(),
            "the line this is built from has spans to drop"
        );
        let moved = Line {
            kind: l.kind,
            moved: true,
            old_no: l.old_no,
            new_no: l.new_no,
            text: l.text.clone(),
            spans: l.spans.clone(),
            tokens: l.tokens.clone(),
        };
        pieces(&moved, 0..moved.text.len(), &mut out);
        assert!(out.iter().all(|p| !p.word));
    }
}
