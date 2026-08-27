//! A diff as a list of rows, and the order they are drawn in.
//!
//! [`prepared`](crate::prepared) stops one step short of a frontend: it hands
//! back files, hunks and lines, and every frontend then flattens the three into
//! one uniform list so that a 714k-line diff scrolls through a virtualized list
//! however large it is. That flattening, the wrap index over it and the order
//! table that maps a row on screen back to a line were written three times —
//! once in `gitten-shell`, once in `gitten-web`, once about to be — which is what
//! *don't put logic in `shell/` that `cli/` would have to duplicate* is warning
//! about. So it is here, and a frontend is left with drawing.
//!
//! # The two row counts
//!
//! They are not the same number and confusing them is the bug this module is
//! shaped to prevent.
//!
//! A **logical** row is a thing in the diff: a file header, a hunk header, a
//! line. [`Flat`] stores one per row and every index it takes is one of these.
//!
//! A **visual** row is a row on screen. A line too wide for the window is *n*
//! visual rows and never one taller one, because a uniform row height is what
//! makes the list virtualize — see [`wrap`](crate::wrap) for why that constraint
//! reaches all the way back here. [`Ordered::order`] holds one [`RowRef`] per
//! visual row, and `seg` says which row of its logical one it is.
//!
//! Everything above wrapping — the edit script, the hunk numbers,
//! [`align`](crate::align), the spans, the tokens — addresses logical rows. A
//! reading position is anchored to one too, which is what lets a reflow put you
//! back on the line you were on rather than at the same proportion of a diff
//! that just changed length.
//!
//! # What a frontend still owns
//!
//! Drawing, and how it measures a column. [`Present`] is the half of a
//! presentation that has nothing to do with a UI: which files it claims, how
//! many rows it holds and how wide they are. The frontend's own trait adds
//! `render`, whose return type is a UI element and is exactly why that trait
//! cannot live here.

use crate::prepared::{prepare, File, Line, Prepared};
use crate::syntax::Highlighter;
use crate::wrap::{Wrap, Wrapped};
use crate::FileDiff;
use std::ops::Range;
use std::time::Duration;

// ------------------------------------------------------------------- the rows

/// One logical row of a diff.
///
/// A file header and a hunk header are rows in the list rather than containers
/// around it: nesting is what forces a frontend to keep two coordinate systems,
/// and there is nothing a header does that a row cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    File {
        path: String,
        adds: usize,
        dels: usize,
    },
    Hunk(String),
    Line(Line),
}

impl Row {
    /// The text this row draws, for measuring and for wrapping.
    ///
    /// A header's text is its own, and it never wraps — [`Flat::reflow`] passes
    /// a budget of zero for it, which is how [`Wrapped`] is told not to break a
    /// line. A parallel table of which rows are lines is the other way to write
    /// that and is a second thing to keep in step.
    pub fn text(&self) -> &str {
        match self {
            Row::File { path, .. } => path,
            Row::Hunk(h) => h,
            Row::Line(l) => &l.text,
        }
    }

    pub fn line(&self) -> Option<&Line> {
        match self {
            Row::Line(l) => Some(l),
            _ => None,
        }
    }
}

/// A file's place in the flat row list: what a jump list or a sidebar needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub adds: usize,
    pub dels: usize,
    /// Which **logical** row is this file's header. Logical rather than visual
    /// because reflow moves the visual one and this would then need
    /// invalidating on every resize; [`Ordered::visual`] converts on demand.
    pub row: usize,
}

/// Which hunk each logical row belongs to, for the verbs that act on one.
///
/// One entry per hunk — not per row, which is the difference between a table
/// that grows with a 714k-line diff and one that grows with its hunk count.
/// Every hunk-shaped presentation builds its rows in hunk order (a file
/// header, then each hunk's header and lines), so recording a span at build
/// time is one push per hunk; reading it back is a binary search.
///
/// The address is `(file, hunk)` and not a path: a span is geometry, and the
/// presentation that recorded it spells the file's name in its own
/// [`Present::files`] list, where `file` is an index. Out-of-range and
/// malformed maps degrade to "no hunk here"; a span that claims a row it does
/// not own is trusted as claimed — the window's own hunk map runs on the same
/// contract, which is why presentation authors record exact spans.
#[derive(Debug, Default)]
pub struct Hunks {
    spans: Vec<HunkSpan>,
}

#[derive(Debug)]
struct HunkSpan {
    /// First logical row of the hunk, inclusive — its header row.
    start: usize,
    /// How many logical rows the hunk spans, header included.
    rows: usize,
    /// The file, numbered as the recording presentation's own
    /// [`Present::files`] order does, and the hunk within it.
    file: usize,
    hunk: usize,
}

impl Hunks {
    /// Records one hunk occupying logical rows `start..start+rows`.
    ///
    /// `rows` of zero is refused rather than stored: a span that addresses
    /// nothing would answer for a row it does not cover, and the honest answer
    /// for a hunk with no rows is that there is no hunk there. Spans arrive in
    /// ascending order — the caller walks the diff as it builds it — which is
    /// what [`Hunks::at`]'s binary search assumes.
    pub fn record(&mut self, start: usize, rows: usize, file: usize, hunk: usize) {
        if rows == 0 {
            return;
        }
        self.spans.push(HunkSpan {
            start,
            rows,
            file,
            hunk,
        });
    }

    /// The hunk under logical row `index`, or nothing for the gaps between
    /// hunks — today only the file headers. A row a recorded span covers is
    /// answered with that span's own claim, unverified: see [`Hunks`].
    pub fn at(&self, index: usize) -> Option<(usize, usize)> {
        let i = self.spans.partition_point(|s| s.start <= index);
        let s = self.spans.get(i.checked_sub(1)?)?;
        (index < s.start + s.rows).then_some((s.file, s.hunk))
    }
}

/// The rows of every file a presentation claimed, flat, with a wrap index.
///
/// The storage a [`Present`] implementation is built out of. Both shipped
/// presentations in every frontend hold one of these; what differs between them
/// is what they draw with it.
#[derive(Debug, Default)]
pub struct Flat {
    rows: Vec<Row>,
    files: Vec<Entry>,
    /// Which hunk every logical row belongs to. Recorded by [`Flat::push`],
    /// because that is the one walk that knows where each hunk starts.
    hunks: Hunks,
    /// Rows that are part of a block that moved. Counted because move detection
    /// finding nothing and move detection being switched off look identical on
    /// screen — see [`Flat::report`].
    moved: usize,
    /// Where each row's text breaks, indexed by logical row. Headers are in it
    /// too, with no breaks, so nothing translates between two numbering schemes.
    wrapped: Wrapped,
    /// The budget and the policy `wrapped` was built for, so a resize that
    /// changes neither costs two comparisons.
    cols: usize,
    wrap: &'static str,
}

impl Flat {
    /// Appends one file's rows: a header, then a header and its lines per hunk.
    ///
    /// Each hunk's logical span — its header row through its last line — is
    /// recorded here, because this is the one walk that knows where every hunk
    /// starts without a second pass over the rows. The file index is this
    /// flat's own file order, which [`Flat::files`] spells.
    pub fn push(&mut self, f: File) {
        let file = self.files.len();
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
        for (hunk, h) in f.hunks.into_iter().enumerate() {
            // The span opens on the header row about to be pushed and closes
            // after the hunk's last line. A hunk without lines spans its
            // header alone — still one row, still a hunk on screen.
            self.hunks
                .record(self.rows.len(), 1 + h.lines.len(), file, hunk);
            self.rows.push(Row::Hunk(h.header));
            for l in h.lines {
                self.moved += l.moved as usize;
                self.rows.push(Row::Line(l));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn get(&self, index: usize) -> Option<&Row> {
        self.rows.get(index)
    }

    pub fn files(&self) -> &[Entry] {
        &self.files
    }

    /// The hunk under logical row `index`, or nothing for the gaps between
    /// hunks — today only the file headers. See [`Hunks::at`] for the address.
    pub fn hunk_at(&self, index: usize) -> Option<(usize, usize)> {
        self.hunks.at(index)
    }

    pub fn moved(&self) -> usize {
        self.moved
    }

    /// How many visual rows logical row `index` occupies. One for everything
    /// that fits, which is nearly everything.
    pub fn visual_rows(&self, index: usize) -> usize {
        self.wrapped.rows(index)
    }

    /// The bytes of row `index` that visual row `seg` draws.
    pub fn range(&self, index: usize, seg: usize) -> Range<usize> {
        match self.rows.get(index) {
            Some(row) => self.wrapped.range(index, seg, row.text()),
            None => 0..0,
        }
    }

    /// The text visual row `seg` of logical row `index` actually draws.
    pub fn piece(&self, index: usize, seg: usize) -> &str {
        match self.rows.get(index) {
            Some(row) => &row.text()[self.wrapped.range(index, seg, row.text())],
            None => "",
        }
    }

    /// Rebuilds the break table for a new column budget, and says whether
    /// anything moved.
    ///
    /// The budget arrives from the frontend, because how wide a row may get is a
    /// property of what is drawing it and `core` cannot know it. The early
    /// return is what makes a resize that does not cross a character boundary
    /// free — without it, every pixel of a drag rescans the whole diff to be
    /// told nothing changed.
    ///
    /// Headers get a budget of zero, which is how [`Wrapped`] is told never to
    /// break a line: a hunk header wrapped over two rows says nothing the one
    /// row did not.
    pub fn reflow(&mut self, cols: usize, wrap: &dyn Wrap) -> bool {
        if cols == self.cols && wrap.name() == self.wrap {
            return false;
        }
        self.cols = cols;
        self.wrap = wrap.name();
        self.wrapped = Wrapped::build(
            self.rows.iter().map(|r| match r {
                Row::Line(l) => (l.text.as_ref(), cols),
                _ => (r.text(), 0),
            }),
            wrap,
        );
        true
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Which wrap the current break table came from. `""` before the first
    /// reflow, when every row is one row and the whole of its text.
    pub fn wrap_name(&self) -> &'static str {
        self.wrap
    }

    /// Breaks a [`Wrap`] produced that were thrown away for breaking the rules
    /// the trait documents.
    pub fn rejected(&self) -> usize {
        self.wrapped.rejected()
    }

    /// What this has to say on a stats overlay, or nothing.
    ///
    /// Both halves are here because both are invisible otherwise: move
    /// detection that found nothing looks exactly like move detection switched
    /// off, and a wrap whose every break was rejected looks exactly like a wrap
    /// with nothing to do.
    pub fn report(&self) -> String {
        let mut out = match self.moved {
            0 => String::new(),
            n => format!("{n} moved"),
        };
        if self.rejected() > 0 {
            if !out.is_empty() {
                out.push_str(" · ");
            }
            out.push_str(&format!(
                "{} invalid breaks from {}",
                self.rejected(),
                self.wrap
            ));
        }
        out
    }
}

// ------------------------------------------------------------------- the seam

/// The half of a presentation that does not draw.
///
/// A frontend's own trait extends this with a `render` that returns a UI
/// element, which is the one part that cannot live in `core`. Everything above
/// it — claiming files, holding rows, counting how many rows a line takes and
/// how wide they are — is the same work in a window, a browser and a terminal,
/// and [`assemble`] and [`expand`] drive it without knowing which.
///
/// `reflow` is deliberately *not* here. A presentation owns the conversion from
/// whatever its frontend measures in — pixels, columns — to a column budget,
/// because it owns the furniture it draws around the text.
pub trait Present {
    /// Whether this implementation wants the file. The built-in claims
    /// everything; the last registered claimant wins, so a specialist can take
    /// `.md` without the generalist having to know it exists.
    fn claims(&self, path: &str) -> bool;

    /// How many logical rows it currently holds.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends the rows for one file, which arrives clipped, intraline-diffed
    /// and highlighted. An implementation draws; it does not redo any of that.
    fn build(&mut self, file: File);

    /// How many visual rows logical row `index` occupies at the current wrap.
    /// Defaulted, so a presentation that does not wrap is exactly as long as it
    /// was before wrapping existed.
    fn rows(&self, _index: usize) -> usize {
        1
    }

    /// Width of one visual row, in whatever unit the frontend measures columns
    /// in. Only ever used to find the widest row, so an approximation is fine —
    /// and zero, the default, means "never the widest".
    fn width(&self, _index: usize, _seg: usize) -> usize {
        0
    }

    /// The files this presentation holds, in order, for a jump list.
    ///
    /// Defaulted to none rather than required, because a presentation that is
    /// not file-shaped has nothing honest to say here — and a caller offering
    /// "jump to next file" over an empty list is a key that does nothing, which
    /// is the right answer rather than a special case.
    fn files(&self) -> &[Entry] {
        &[]
    }

    /// The hunk under logical row `index`, as `(file, hunk)` — the file
    /// numbered as this presentation's own [`Present::files`] order spells it.
    ///
    /// Defaulted to none, for the same reason `files` is: a presentation that
    /// draws no hunks has nothing actionable under the keyboard, and the verbs
    /// that act on one say so rather than guess. Both shipped implementations
    /// record their spans as they build; an extension records its own the same
    /// way, and a recorded index that answers nothing on [`Present::files`]
    /// degrades to "no hunk here" rather than to the wrong hunk.
    fn hunk_at(&self, _index: usize) -> Option<(usize, usize)> {
        None
    }
}

/// So `&[Box<dyn FrontendTrait>]` works wherever `&[impl Present]` is wanted,
/// which is every call site in every frontend: their traits have this one as a
/// supertrait, and `Box` is the only shape a registry of them comes in.
impl<T: Present + ?Sized> Present for Box<T> {
    fn claims(&self, path: &str) -> bool {
        (**self).claims(path)
    }
    fn len(&self) -> usize {
        (**self).len()
    }
    fn build(&mut self, file: File) {
        (**self).build(file)
    }
    fn rows(&self, index: usize) -> usize {
        (**self).rows(index)
    }
    fn width(&self, index: usize, seg: usize) -> usize {
        (**self).width(index, seg)
    }
    fn files(&self) -> &[Entry] {
        (**self).files()
    }
    fn hunk_at(&self, index: usize) -> Option<(usize, usize)> {
        (**self).hunk_at(index)
    }
}

// ----------------------------------------------------------- the order table

/// 8 bytes per visual row: which implementation owns it, where in that
/// implementation's own storage it sits, and which row of that logical row this
/// one is.
///
/// The rows themselves are never boxed — at 700k rows that is 700k allocations
/// to chase on every scroll. `seg` fits in the two bytes `owner` and `index`
/// left over, so wrapping cost the table nothing; it caps a line at 65,535 rows,
/// which a clip at 2000 characters and a floor of 8 columns put out of reach by
/// a factor of 260.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowRef {
    pub owner: u16,
    pub seg: u16,
    pub index: u32,
}

impl RowRef {
    /// The logical row this one is part of — what survives a reflow, and
    /// therefore what a reading position is anchored to.
    pub fn logical(self) -> (u16, u32) {
        (self.owner, self.index)
    }
}

/// An order table, and the two things worth computing while walking it.
#[derive(Debug, Default)]
pub struct Ordered {
    pub order: Vec<RowRef>,
    /// Index into `order` of the widest visual row. What a list that measures
    /// one row to decide its scrollable width has to be pointed at, and what a
    /// terminal bounds a horizontal scroll with.
    pub widest: usize,
    /// Where the anchor's logical row landed, so a reflow keeps your place.
    pub anchor: usize,
}

impl Ordered {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The first visual row of a logical one. Linear, so it is for a jump and
    /// not for a scroll: a frontend scrolling by rows already holds the index.
    pub fn visual(&self, owner: u16, index: u32) -> Option<usize> {
        self.order
            .iter()
            .position(|r| r.logical() == (owner, index))
    }
}

/// Expands one entry per **logical** row into one per **visual** row.
///
/// `logical` may already be expanded: consecutive entries with the same owner
/// and index are one logical row, and an index is unique within an owner, so the
/// previous table is its own source of truth. That is the whole reason a reflow
/// needs no second table remembering the unwrapped shape — 8 bytes a row, once,
/// however many times the window is dragged.
pub fn expand<P: Present>(logical: &[RowRef], owners: &[P], anchor: Option<RowRef>) -> Ordered {
    let mut order: Vec<RowRef> = Vec::with_capacity(logical.len());
    let (mut widest, mut widest_at) = (0usize, 0usize);
    let mut found = 0usize;
    let mut i = 0;
    while i < logical.len() {
        let r = logical[i];
        while i < logical.len() && logical[i].logical() == r.logical() {
            i += 1;
        }
        let Some(rows) = owners.get(r.owner as usize) else {
            continue;
        };
        if anchor.map(RowRef::logical) == Some(r.logical()) {
            found = order.len();
        }
        // Clamped at both ends: zero rows would drop a line out of the diff
        // silently, and `seg` is a `u16`.
        let n = rows.rows(r.index as usize).clamp(1, u16::MAX as usize);
        for seg in 0..n {
            let w = rows.width(r.index as usize, seg);
            if w > widest {
                (widest, widest_at) = (w, order.len());
            }
            order.push(RowRef {
                owner: r.owner,
                seg: seg as u16,
                index: r.index,
            });
        }
    }
    Ordered {
        order,
        widest: widest_at,
        anchor: found,
    }
}

/// What one pass of the pipeline produced, beside the rows themselves.
#[derive(Debug)]
pub struct Assembled {
    pub ordered: Ordered,
    pub files: usize,
    /// CPU time summed across `prepare`'s workers, not wall clock — see
    /// [`Prepared::intraline`](crate::prepared::Prepared::intraline).
    pub intraline: Duration,
    /// CPU time summed across `prepare`'s workers. See [`Self::intraline`].
    pub syntax: Duration,
    /// How many workers `prepare` used.
    pub threads: usize,
}

/// Runs the shared pipeline and hands every file to the presentation that
/// claimed it.
///
/// This is the whole of what a frontend does between a parsed diff and its first
/// frame, and none of it is frontend-specific: one [`prepare`] pass, then a
/// claim per file, then the order table. The one number it takes is
/// `max_line_chars`, because how wide a row may get before it is clipped is a
/// rendering budget rather than a fact about diffs.
///
/// Nothing wraps yet — no presentation has been given a width — so the table it
/// returns is one row per line, and the first frame reflows and expands it
/// again.
///
/// `owners` empty is an empty diff rather than a panic; a frontend supplies its
/// own fallback presentation, because `core` cannot construct one.
pub fn assemble<P: Present>(
    files: &[FileDiff],
    hl: &dyn Highlighter,
    max_line_chars: usize,
    owners: &mut [P],
) -> Assembled {
    let Prepared {
        files: prepared,
        intraline,
        syntax,
        threads,
    } = prepare(files, hl, max_line_chars);
    let count = prepared.len();
    let mut logical: Vec<RowRef> = Vec::new();

    for f in prepared {
        // Last claimant wins, so a specialist registered after the generalist
        // takes the file without the generalist having to know it exists. `0`
        // when nothing claims it at all, which only happens if a frontend
        // registered a presentation that claims nothing.
        let Some(owner) = owners
            .iter()
            .enumerate()
            .rev()
            .find(|(_, r)| r.claims(&f.path))
            .map(|(i, _)| i)
            .or(if owners.is_empty() { None } else { Some(0) })
        else {
            continue;
        };
        let r = &mut owners[owner];
        let first = r.len();
        r.build(f);
        for index in first..r.len() {
            logical.push(RowRef {
                owner: owner as u16,
                seg: 0,
                index: index as u32,
            });
        }
    }

    Assembled {
        ordered: expand(&logical, owners, None),
        files: count,
        intraline,
        syntax,
        threads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Host;
    use crate::parse_unified_diff;
    use crate::wrap::{Char, Off, Word};
    use crate::LineKind;

    const DIFF: &str = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,3 @@
 fn one() {}
-let x = 1;
+let x = 2;
 fn two() {}
diff --git a/b.md b/b.md
--- a/b.md
+++ b/b.md
@@ -1,1 +1,1 @@
-# a heading that is quite a lot longer than eight characters
+# another heading that is also longer than eight characters
";

    /// A presentation with no drawing in it, which is all [`assemble`] and
    /// [`expand`] ever needed — and the proof that neither knows what a UI is.
    #[derive(Default)]
    struct Text {
        flat: Flat,
        only: Option<&'static str>,
    }

    impl Present for Text {
        fn claims(&self, path: &str) -> bool {
            match self.only {
                Some(ext) => path.ends_with(ext),
                None => true,
            }
        }
        fn len(&self) -> usize {
            self.flat.len()
        }
        fn build(&mut self, file: File) {
            self.flat.push(file);
        }
        fn rows(&self, index: usize) -> usize {
            self.flat.visual_rows(index)
        }
        fn width(&self, index: usize, seg: usize) -> usize {
            self.flat.piece(index, seg).trim_end().chars().count()
        }
        fn files(&self) -> &[Entry] {
            self.flat.files()
        }
        fn hunk_at(&self, index: usize) -> Option<(usize, usize)> {
            self.flat.hunk_at(index)
        }
    }

    fn one_owner() -> (Vec<Box<dyn Present>>, Assembled) {
        let host = Host::new();
        let mut owners: Vec<Box<dyn Present>> = vec![Box::new(Text::default())];
        let a = assemble(&parse_unified_diff(DIFF), &host.syntax, 2000, &mut owners);
        (owners, a)
    }

    #[test]
    fn a_file_becomes_a_header_a_hunk_header_and_a_row_per_line() {
        let mut flat = Flat::default();
        let host = Host::new();
        for f in prepare(&parse_unified_diff(DIFF), &host.syntax, 2000).files {
            flat.push(f);
        }
        // 1 + 1 + 4 for a.rs, 1 + 1 + 2 for b.md.
        assert_eq!(flat.len(), 10);
        assert!(matches!(flat.get(0), Some(Row::File { .. })));
        assert!(matches!(flat.get(1), Some(Row::Hunk(_))));
        assert_eq!(flat.files().len(), 2);
        assert_eq!(flat.files()[1].row, 6);
        assert_eq!(flat.files()[1].path, "b.md");
    }

    #[test]
    fn the_pipeline_reaches_the_rows_it_produced() {
        // Routing, the intraline pass and the syntax pass all ran, and a
        // presentation did none of them.
        let (owners, a) = one_owner();
        assert_eq!(a.files, 2);
        assert_eq!(a.ordered.len(), 10, "nothing wraps before a reflow");
        assert!(a.ordered.order.iter().all(|r| r.owner == 0 && r.seg == 0));
        assert_eq!(owners[0].len(), 10);
    }

    #[test]
    fn the_widest_row_is_found_before_anything_is_drawn() {
        let (_, a) = one_owner();
        // The long heading in b.md, which is row 8 — the first line of the
        // second file's only hunk.
        assert_eq!(a.ordered.order[a.ordered.widest].index, 8);
    }

    #[test]
    fn a_jump_list_reaches_the_files_a_presentation_claimed() {
        let (owners, _) = one_owner();
        let files = owners[0].files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].path, "b.md");
        assert_eq!(files[1].row, 6);
        // A presentation that says nothing offers an empty list rather than a
        // wrong one.
        struct Silent;
        impl Present for Silent {
            fn claims(&self, _: &str) -> bool {
                false
            }
            fn len(&self) -> usize {
                0
            }
            fn build(&mut self, _: File) {}
        }
        assert!(Silent.files().is_empty());
    }

    #[test]
    fn the_last_registered_claimant_wins() {
        let host = Host::new();
        let mut owners: Vec<Box<dyn Present>> = vec![
            Box::new(Text::default()),
            Box::new(Text {
                only: Some(".md"),
                ..Default::default()
            }),
        ];
        let a = assemble(&parse_unified_diff(DIFF), &host.syntax, 2000, &mut owners);
        assert_eq!(owners[0].len(), 6, "the generalist kept a.rs");
        assert_eq!(owners[1].len(), 4, "the specialist took b.md");
        // Both owners appear in one table, in file order.
        assert_eq!(a.ordered.order.first().unwrap().owner, 0);
        assert_eq!(a.ordered.order.last().unwrap().owner, 1);
    }

    #[test]
    fn no_owners_is_an_empty_diff_rather_than_a_panic() {
        let host = Host::new();
        let mut owners: Vec<Box<dyn Present>> = Vec::new();
        let a = assemble(&parse_unified_diff(DIFF), &host.syntax, 2000, &mut owners);
        assert!(a.ordered.is_empty());
        assert_eq!(a.files, 2);
    }

    #[test]
    fn a_reflow_to_the_same_budget_reports_no_change() {
        let mut flat = Flat::default();
        let host = Host::new();
        for f in prepare(&parse_unified_diff(DIFF), &host.syntax, 2000).files {
            flat.push(f);
        }
        assert!(flat.reflow(20, &Word));
        assert!(!flat.reflow(20, &Word));
        assert!(flat.reflow(20, &Char));
        assert!(flat.reflow(0, &Char));
        assert_eq!(flat.cols(), 0);
        assert_eq!(flat.wrap_name(), "char");
    }

    #[test]
    fn a_header_never_wraps_however_narrow_the_budget() {
        // The reason headers carry a budget of zero: a hunk header split over
        // two rows says nothing the one row did not.
        let mut flat = Flat::default();
        let host = Host::new();
        for f in prepare(&parse_unified_diff(DIFF), &host.syntax, 2000).files {
            flat.push(f);
        }
        flat.reflow(8, &Char);
        for (i, row) in flat.rows().iter().enumerate() {
            match row {
                Row::Line(_) => {}
                _ => assert_eq!(flat.visual_rows(i), 1, "row {i} ({row:?}) wrapped"),
            }
        }
        assert!(flat.visual_rows(8) > 1, "the long heading did not wrap");
    }

    #[test]
    fn wrapping_adds_visual_rows_and_every_one_maps_back_to_its_line() {
        let host = Host::new();
        let mut owners = vec![Text::default()];
        let a = assemble(&parse_unified_diff(DIFF), &host.syntax, 2000, &mut owners);
        owners[0].flat.reflow(8, &Word);
        let re = expand(&a.ordered.order, &owners, None);
        assert!(re.len() > a.ordered.len());

        // Every logical row's segments are consecutive and start at zero, which
        // is what a renderer slicing `range(index, seg)` depends on.
        let mut seen = vec![Vec::new(); owners[0].len()];
        for r in &re.order {
            seen[r.index as usize].push(r.seg as usize);
        }
        for (i, segs) in seen.iter().enumerate() {
            assert_eq!(*segs, (0..segs.len()).collect::<Vec<_>>(), "row {i}");
            assert_eq!(segs.len(), owners[0].rows(i), "row {i}");
        }
        // ...and the pieces of a wrapped line reassemble it, minus the
        // whitespace `Word` dropped.
        let text = owners[0].flat.get(8).unwrap().text().to_string();
        let joined: String = (0..owners[0].rows(8))
            .map(|s| owners[0].flat.piece(8, s))
            .collect();
        assert_eq!(joined.replace(' ', ""), text.replace(' ', ""));
    }

    #[test]
    fn expanding_an_already_expanded_table_is_idempotent() {
        // The property that lets a reflow use the previous table as its own
        // source of truth, with no second table remembering the flat shape.
        let host = Host::new();
        let mut owners = vec![Text::default()];
        let a = assemble(&parse_unified_diff(DIFF), &host.syntax, 2000, &mut owners);
        owners[0].flat.reflow(8, &Word);
        let once = expand(&a.ordered.order, &owners, None);
        let twice = expand(&once.order, &owners, None);
        assert_eq!(once.order, twice.order);
    }

    #[test]
    fn a_reflow_keeps_the_line_you_were_on() {
        let host = Host::new();
        let mut owners = vec![Text::default()];
        let a = assemble(&parse_unified_diff(DIFF), &host.syntax, 2000, &mut owners);
        // Reading the second file's heading, before anything wrapped.
        let anchor = a.ordered.order[8];
        owners[0].flat.reflow(8, &Word);
        let re = expand(&a.ordered.order, &owners, Some(anchor));
        assert_eq!(re.order[re.anchor].logical(), anchor.logical());
        assert_eq!(re.order[re.anchor].seg, 0, "landed mid-line");
        assert!(
            re.anchor > 8,
            "the rows above it wrapped and it did not move"
        );
    }

    #[test]
    fn off_leaves_every_row_a_single_row() {
        let host = Host::new();
        let mut owners = vec![Text::default()];
        let a = assemble(&parse_unified_diff(DIFF), &host.syntax, 2000, &mut owners);
        owners[0].flat.reflow(8, &Off);
        let re = expand(&a.ordered.order, &owners, None);
        assert_eq!(re.len(), owners[0].len());
        assert_eq!(
            owners[0].flat.piece(8, 0),
            owners[0].flat.get(8).unwrap().text()
        );
    }

    #[test]
    fn a_row_or_a_segment_past_the_end_is_empty_rather_than_a_panic() {
        let mut flat = Flat::default();
        let host = Host::new();
        for f in prepare(&parse_unified_diff(DIFF), &host.syntax, 2000).files {
            flat.push(f);
        }
        flat.reflow(8, &Word);
        assert_eq!(flat.piece(9999, 0), "");
        assert_eq!(flat.range(9999, 0), 0..0);
        assert_eq!(flat.visual_rows(9999), 1);
        // A segment past the end of a real row is the whole line, which is what
        // `Wrapped` documents and what a stale order table would ask for.
        assert!(!flat.piece(8, 9999).is_empty());
    }

    #[test]
    fn the_report_names_both_things_that_are_otherwise_invisible() {
        let mut flat = Flat::default();
        let host = Host::new();
        for f in prepare(&parse_unified_diff(DIFF), &host.syntax, 2000).files {
            flat.push(f);
        }
        assert_eq!(flat.report(), "", "nothing moved in this diff");
        assert_eq!(flat.moved(), 0);

        struct Bad;
        impl Wrap for Bad {
            fn name(&self) -> &'static str {
                "bad"
            }
            fn breaks(&self, text: &str, _cols: usize, out: &mut Vec<crate::wrap::Break>) {
                out.push(crate::wrap::Break::hard(text.len() + 1000));
            }
        }
        flat.reflow(8, &Bad);
        assert!(flat.rejected() > 0);
        assert!(
            flat.report().contains("invalid breaks from bad"),
            "{}",
            flat.report()
        );
    }

    #[test]
    fn a_moved_line_is_counted_as_it_is_stored() {
        let mut flat = Flat::default();
        let host = Host::new();
        let mut files = prepare(&parse_unified_diff(DIFF), &host.syntax, 2000).files;
        for l in files[0].hunks[0].lines.iter_mut() {
            l.moved = l.kind != LineKind::Context;
        }
        for f in files {
            flat.push(f);
        }
        assert_eq!(flat.moved(), 2);
        assert_eq!(flat.report(), "2 moved");
    }

    // ---------------------------------------------------------------- hunks

    #[test]
    fn flat_records_exact_hunk_logical_ranges() {
        // `DIFF`: rows 0–5 are a.rs (header, hunk header, four lines) and
        // 6–9 are b.md (header, hunk header, two lines).
        let mut flat = Flat::default();
        let host = Host::new();
        for f in prepare(&parse_unified_diff(DIFF), &host.syntax, 2000).files {
            flat.push(f);
        }
        assert_eq!(flat.len(), 10);
        // A file header owns no hunk: the spans open below it.
        assert_eq!(flat.hunk_at(0), None);
        // Header, first line and last line of the first file's hunk.
        assert_eq!(flat.hunk_at(1), Some((0, 0)));
        assert_eq!(flat.hunk_at(2), Some((0, 0)), "the hunk's first line");
        assert_eq!(flat.hunk_at(5), Some((0, 0)), "the hunk's last line");
        // The gap between the files, and the second file's own hunk.
        assert_eq!(flat.hunk_at(6), None, "the second file's header");
        assert_eq!(flat.hunk_at(7), Some((1, 0)));
        assert_eq!(flat.hunk_at(9), Some((1, 0)), "the last line of the diff");
        assert_eq!(flat.hunk_at(10), None, "one past the end");
        // A row index beyond the table is nothing, as everywhere else here.
        assert_eq!(flat.hunk_at(9999), None);
    }

    #[test]
    fn two_hunks_abut_without_one_swallowing_the_other() {
        // Context one on both sides makes the hunks touch: the last row of
        // one and the header row of the next are neighbours, and a search
        // that rounds the wrong way hands one of them to the other.
        let raw = "\
diff --git a/a.rs b/a.rs
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
@@ -4,3 +4,3 @@
 four
-five
+FIVE
 six
";
        let mut flat = Flat::default();
        let host = Host::new();
        for f in prepare(&parse_unified_diff(raw), &host.syntax, 2000).files {
            flat.push(f);
        }
        // Rows 1–5 are hunk 0, rows 6–10 are hunk 1, and nothing lies between.
        assert_eq!(flat.hunk_at(5), Some((0, 0)), "hunk 0's last line");
        assert_eq!(flat.hunk_at(6), Some((0, 1)), "hunk 1's header");
        assert_eq!(flat.hunk_at(10), Some((0, 1)), "hunk 1's last line");
        assert_eq!(flat.hunk_at(11), None);
    }

    #[test]
    fn hunk_lookup_survives_visual_wrapping() {
        // The spans are in logical rows, so a wrapped line's segments all
        // answer for the one hunk their line belongs to — that is the whole
        // reason the address is logical and not visual.
        let host = Host::new();
        let mut owners = vec![Text::default()];
        let a = assemble(&parse_unified_diff(DIFF), &host.syntax, 2000, &mut owners);
        owners[0].flat.reflow(8, &Word);
        let re = expand(&a.ordered.order, &owners, None);
        assert!(re.len() > a.ordered.len(), "nothing wrapped");

        let mut answers: Vec<Option<(usize, usize)>> = Vec::new();
        for r in &re.order {
            let answer = owners[0].hunk_at(r.index as usize);
            // Consecutive segments of one logical row agree.
            if let Some(previous) = answers.last() {
                let before = re.order[answers.len() - 1];
                if before.logical() == r.logical() {
                    assert_eq!(*previous, answer, "row {:?}", r);
                }
            }
            answers.push(answer);
        }
        // The long heading — logical row 8, inside b.md's hunk — wrapped, and
        // every one of its rows still names the same hunk.
        assert!(owners[0].rows(8) > 1, "the heading did not wrap");
        let segments: Vec<Option<(usize, usize)>> = re
            .order
            .iter()
            .filter(|r| r.index == 8)
            .map(|r| owners[0].hunk_at(r.index as usize))
            .collect();
        assert!(segments.len() > 1, "{segments:?}");
        assert!(segments.iter().all(|s| *s == Some((1, 0))), "{segments:?}");
    }

    #[test]
    fn an_unaware_presentation_has_no_actionable_hunk() {
        // Defaulted on the trait, so this is what an extension gets for free
        // rather than a special case in the caller: its rows are honestly
        // nobody's hunk.
        struct Bare(usize);
        impl Present for Bare {
            fn claims(&self, _: &str) -> bool {
                true
            }
            fn len(&self) -> usize {
                self.0
            }
            fn build(&mut self, f: File) {
                self.0 += 1 + f.hunks.iter().map(|h| 1 + h.lines.len()).sum::<usize>();
            }
        }
        let mut bare = Bare(0);
        let host = Host::new();
        for f in prepare(&parse_unified_diff(DIFF), &host.syntax, 2000).files {
            bare.build(f);
        }
        for row in 0..bare.len() {
            assert_eq!(bare.hunk_at(row), None, "row {row} claimed a hunk");
        }
        // And through the `Box` every frontend's registry stores them in.
        let boxed: Box<dyn Present> = Box::new(Bare(3));
        assert_eq!(boxed.hunk_at(1), None);
    }

    #[test]
    fn a_zero_length_span_is_refused_rather_than_recorded() {
        // A span that addresses nothing would answer for a row it does not
        // cover; the honest answer for it is that there is no hunk there.
        let mut hunks = Hunks::default();
        hunks.record(4, 0, 0, 0);
        assert_eq!(hunks.at(4), None);
        // A real span beside the refused one still answers.
        hunks.record(5, 2, 1, 1);
        assert_eq!(hunks.at(4), None, "the refusal did not shift a span");
        assert_eq!(hunks.at(5), Some((1, 1)));
        assert_eq!(hunks.at(6), Some((1, 1)));
        assert_eq!(hunks.at(7), None);
    }
}
