//! A list of things, as flat rows — the pane shape every client draws.
//!
//! Whatever it is a list *of*, a repository pane is the same list
//! underneath: entries under named sections, a heading over a group only
//! while the group has something in it, a cursor that never rests on one, a
//! `/` filter that keeps a group's heading while it keeps a row of it, and
//! one destructive verb per pane that asks twice. That shape was written
//! once per pane per client — the working tree, branches, stashes, tags,
//! remotes, the reflog, worktrees, commits — which is the test
//! `docs/architecture.md` sets for something belonging in `core`. So it is
//! written once, here, and a pane is left with its drawing and its keys.
//!
//! # The row model
//!
//! [`Row`] is one flat row: a [`Row::Heading`] naming its section with its
//! count spelled, or a [`Row::Entry`] — the only half a verb or a query has
//! anything to say to. [`flatten`] turns `(section, entries)` pairs into
//! rows, skipping an empty group outright rather than drawing its heading.
//! A section is *generic* — a name plus its entries — so each pane maps its
//! own domain onto it: the working tree's staged/unstaged/untracked/
//! conflicts, the branches pane's local/remote, or a list with no sections
//! at all, which simply never pushes a heading.
//!
//! # The rest
//!
//! Everything else is what every pane then reimplemented over those rows:
//! which positions the cursor may rest on ([`selectable`],
//! [`selectable_shown`], [`settle_at`], [`settle_shown`]), which rows a
//! query keeps ([`matches`], [`search_rows`], [`identity`],
//! [`normalize_query`], [`filter_note`]), the one-question confirmation slot
//! ([`Armed`]), the label grammar every title strip spells ([`label`],
//! [`counted`], [`question`]) and the status letters both files panes
//! spelled twice ([`Mark`], [`conflict_letters`]).
//!
//! What stays client-side, deliberately: the viewport itself —
//! [`crate::view::Viewport`] is the model every list already holds — the
//! `visible` table's storage, each pane's own entry payload, and every ink.
//! A theme field is a client's to choose, which is why [`Mark`] stops at the
//! letter and the row model carries no colour.

use std::collections::HashSet;
use std::hash::Hash;

use crate::status::{Change, ConflictKind};

// ------------------------------------------------------------------- the rows

/// One flat row of a list pane: a section heading or one entry.
///
/// Flattened once per refresh — never per frame. The heading is furniture
/// over a group: it draws only because the group under it is non-empty, it
/// can never hold the cursor, and a verb aimed at it has nowhere to go. The
/// entry is everything else — the thing a verb names and a query matches.
///
/// `S` is the pane's own section type, mapped here as data; `E` is whatever
/// the row holds — a file, a ref, a stash entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row<S, E> {
    /// A group heading, drawn only because the group under it is non-empty.
    Heading {
        /// Which group — the pane's own section type, for its label and ink.
        section: S,
        /// How many entries sit under it, spelled out once at flatten: the
        /// render path allocates nothing for a count.
        count: String,
    },
    /// One entry of the group — the only half a verb can aim at.
    Entry(E),
}

impl<S, E> Row<S, E> {
    /// Whether this row is furniture — a heading, which no verb can aim at
    /// and the cursor never rests on.
    pub fn is_heading(&self) -> bool {
        matches!(self, Row::Heading { .. })
    }

    /// The entry this row is, when it is one — `None` on a heading.
    pub fn entry(&self) -> Option<&E> {
        match self {
            Row::Entry(e) => Some(e),
            Row::Heading { .. } => None,
        }
    }

    /// The entry, mutably — for a numbering pass.
    pub fn entry_mut(&mut self) -> Option<&mut E> {
        match self {
            Row::Entry(e) => Some(e),
            Row::Heading { .. } => None,
        }
    }

    /// The heading's section and spelled-out count — `None` on an entry.
    pub fn heading(&self) -> Option<(&S, &str)> {
        match self {
            Row::Heading { section, count } => Some((section, count)),
            Row::Entry(_) => None,
        }
    }
}

/// Flattens `(section, entries)` pairs into rows: one heading per group that
/// has anything in it — spelled with its count — then that group's entries.
///
/// An empty group earns no heading: "a section is drawn only when it has
/// something in it" is the rule every pane's flatten agrees on, and skipping
/// here rather than at draw is what keeps a filtered list's headings honest
/// — see [`search_rows`]. A pane that wants a row ahead of its sections — a
/// detached HEAD is a place, not a branch — pushes it first and hands
/// `flatten` the rest.
pub fn flatten<S, E>(sections: impl IntoIterator<Item = (S, Vec<E>)>) -> Vec<Row<S, E>> {
    let mut rows = Vec::new();
    for (section, entries) in sections {
        if entries.is_empty() {
            continue;
        }
        rows.push(Row::Heading {
            section,
            count: entries.len().to_string(),
        });
        rows.extend(entries.into_iter().map(Row::Entry));
    }
    rows
}

/// The entry half of a flattened list, in draw order — headings dropped.
/// What a whole-list count or a section's `paths_in` reads.
pub fn entries<S, E>(rows: &[Row<S, E>]) -> impl Iterator<Item = &E> {
    rows.iter().filter_map(Row::entry)
}

/// The same half, mutably — what [`number`] is written over.
pub fn entries_mut<S, E>(rows: &mut [Row<S, E>]) -> impl Iterator<Item = &mut E> {
    rows.iter_mut().filter_map(Row::entry_mut)
}

/// Numbers the entries from one in draw order, writing each ordinal through
/// `set` — the `{at}/{total}` a status line spells costs one field per row,
/// decided by the same pass that flattened them.
pub fn number<S, E>(rows: &mut [Row<S, E>], set: impl Fn(&mut E, usize)) {
    for (n, e) in entries_mut(rows).enumerate() {
        set(e, n + 1);
    }
}

/// The entry at source position `i`, or `None` on a heading or out of
/// range — `rows.get(i)` plus the only match arm that answers anything.
pub fn entry_at<S, E>(rows: &[Row<S, E>], i: usize) -> Option<&E> {
    rows.get(i)?.entry()
}

/// The row shown position `i` names: `visible[i]` in `rows`. The visible
/// table is the one indirection every filtered list keeps — the cursor and
/// every per-frame reader address *shown* positions, and only the lookup
/// names a source row.
pub fn shown<'a, R>(rows: &'a [R], visible: &[usize], i: usize) -> Option<&'a R> {
    rows.get(*visible.get(i)?)
}

/// [`entry_at`] through the visible table — what `current()` is, in one
/// call: the source row `visible[i]` names, when it is an entry.
pub fn entry_at_shown<'a, S, E>(
    rows: &'a [Row<S, E>],
    visible: &[usize],
    i: usize,
) -> Option<&'a E> {
    shown(rows, visible, i)?.entry()
}

/// How many distinct keys the entries carry — the files panes' "changed"
/// count: one path staged and edited again sits in two sections and is
/// still one change to a person.
pub fn count_distinct<S, E, K>(rows: &[Row<S, E>], key: impl Fn(&E) -> K) -> usize
where
    K: Eq + Hash,
{
    entries(rows).map(key).collect::<HashSet<K>>().len()
}

// ----------------------------------------------------------------- the cursor

/// Whether position `i` names a row the cursor may rest on: in range, and
/// not furniture. The predicate [`Viewport::settle`] runs — a heading is a
/// label over rows and a verb aimed at one has nowhere to go, so every
/// cursor move ends in the walk.
///
/// `heading` is the pane's half of the judgment — which rows are furniture.
/// Over [`Row`]s it is `Row::is_heading`; a list with no headings never
/// calls this.
///
/// [`Viewport::settle`]: crate::view::Viewport::settle
pub fn selectable<R>(rows: &[R], heading: impl Fn(&R) -> bool, i: usize) -> bool {
    rows.get(i).is_some_and(|r| !heading(r))
}

/// [`selectable`] over the shown rows: shown position `i` is judged by the
/// source row `visible[i]` names — the space the cursor addresses under a
/// filter.
pub fn selectable_shown<R>(
    rows: &[R],
    visible: &[usize],
    heading: impl Fn(&R) -> bool,
    i: usize,
) -> bool {
    visible
        .get(i)
        .is_some_and(|&d| selectable(rows, &heading, d))
}

/// Where the cursor comes to rest after a move that landed it on `at`.
///
/// A heading is a fact about the grouping and not a thing a verb can aim
/// at, so the keyboard never stops on one: it steps on in the direction it
/// was going, and only when the heading is the list's edge in that
/// direction — `k` from the first branch onto `LOCAL` — does it settle the
/// other way, which keeps `k` on row zero's heading from reading as
/// "nothing happened" and `G` from resting on a heading with an empty group
/// under it. `dir` is the sign of the move; zero counts as forward.
///
/// The pure-index half of [`Viewport::settle`](crate::view::Viewport::settle)
/// — the answer without the viewport, for a caller whose cursor lives in a
/// `Cell` and is put back with `go_to`.
pub fn settle_at<R>(rows: &[R], heading: impl Fn(&R) -> bool, at: usize, dir: isize) -> usize {
    settle_by(rows.len(), |i| rows.get(i).is_some_and(&heading), at, dir)
}

/// [`settle_at`] with the furniture test lifted out, as a position predicate
/// — so the same walk runs over source rows and over the shown table alike.
pub fn settle_by(len: usize, furniture: impl Fn(usize) -> bool, at: usize, dir: isize) -> usize {
    if !furniture(at) {
        return at;
    }
    let forward = (at + 1..len).find(|&i| !furniture(i));
    let back = (0..at).rev().find(|&i| !furniture(i));
    match dir.is_negative() {
        false => forward.or(back),
        true => back.or(forward),
    }
    .unwrap_or(at)
}

/// [`settle_at`] over the shown rows: the same walk, in the space the cursor
/// addresses — a heading survives a filter only when its group under it
/// does, and the keyboard never rests on one either way.
pub fn settle_shown<R>(
    rows: &[R],
    visible: &[usize],
    heading: impl Fn(&R) -> bool,
    at: usize,
    dir: isize,
) -> usize {
    settle_by(
        visible.len(),
        |i| shown(rows, visible, i).is_some_and(&heading),
        at,
        dir,
    )
}

/// `by` positions on from `at`, wrapping at both ends — what next- and
/// prev-match are. The deliberate opposite of
/// [`Viewport::move_by`](crate::view::Viewport::move_by)'s clamp: a match
/// jump cycles, a cursor move stops, because a list that jumps from the last
/// row to the first loses your place by the whole list and a *hit* has no
/// place to lose.
///
/// An empty list has nowhere to wrap to, and answers `at`.
pub fn wrap_index(at: usize, by: isize, len: usize) -> usize {
    if len == 0 {
        return at;
    }
    (at as isize + by).rem_euclid(len as isize) as usize
}

// ----------------------------------------------------------------- the filter

/// The one matcher, where the rows live: a query matches when the row's
/// text contains it, folded — exactly what the commit list's search does.
pub fn matches(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// The rows a query keeps, as indices into `rows`: an entry whose text
/// contains the needle, folded — plus the heading of every group that still
/// has an entry under it, exactly the groups an empty one drops at
/// [`flatten`]. A filtered list keeps its *non-empty* headings rather than
/// drawing orphan matches under the wrong one, or a heading naming nothing.
///
/// An empty — or whitespace-only — query keeps every row, the
/// [`TextIndex`](crate::search::TextIndex) rule, so a cleared prompt
/// restores the list it filtered and the caller never special-cases it.
/// `text` answers `None` for a row no query names — a detached HEAD is a
/// place, not a branch name: such a row shows unfiltered and a real query
/// drops it. It is a `fn` and not a closure: the borrowed text ties to the
/// row's lifetime, a signature a plain `fn` carries and a `let`-bound
/// closure cannot always be talked into — every pane's text is a field read
/// anyway, which captures nothing.
///
/// The needle is folded once and each row's text per keystroke. For a list
/// big enough that folding per keystroke costs — the commit graph's tens of
/// thousands — [`crate::search::Index`] is the tool instead; this is the
/// pane-scale one.
pub fn search_rows<R>(
    rows: &[R],
    query: &str,
    heading: impl Fn(&R) -> bool,
    text: fn(&R) -> Option<&str>,
) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return identity(rows);
    }
    let mut out = Vec::new();
    let mut pending = None;
    for (i, row) in rows.iter().enumerate() {
        if heading(row) {
            pending = Some(i);
            continue;
        }
        if text(row).is_some_and(|t| t.to_lowercase().contains(&needle)) {
            // The first kept row of a group emits its heading; a group whose
            // every row missed leaves `pending` for the next heading to
            // overwrite, and is never drawn.
            if let Some(h) = pending.take() {
                out.push(h);
            }
            out.push(i);
        }
    }
    out
}

/// The visible table of a list with no query standing — every row, in
/// order. What clearing a filter restores, kept whole because rebuilding it
/// is what `search.clear` and an emptied prompt do.
pub fn identity<R>(rows: &[R]) -> Vec<usize> {
    Vec::from_iter(0..rows.len())
}

/// What a prompt's text means as a standing filter: the trimmed text, and
/// `None` when nothing is left — an empty query is no query, in every pane,
/// so clearing restores the whole list.
pub fn normalize_query(query: &str) -> Option<String> {
    Some(query.trim())
        .filter(|q| !q.is_empty())
        .map(str::to_string)
}

/// What a pane's label appends while a filter stands: `shown` over `total`
/// — `"3/15"`. `None` unfiltered, so a note is drawn only when there is one.
pub fn filter_note(filter: Option<&str>, shown: usize, total: usize) -> Option<String> {
    filter.is_some().then(|| format!("{shown}/{total}"))
}

// --------------------------------------------------------- the two-press verb

/// The one-question confirmation slot every pane keeps: a destructive
/// verb's first press *arms* its target and asks through the status line;
/// a second press on the same target spends the arm and runs; anything else
/// — a cursor move, a wheel, a refresh, an arm aimed elsewhere — drops or
/// moves the question rather than queueing two.
///
/// `T` is whatever names the row under threat: a `(section, path)` pair, a
/// [`Target`](crate::refs::Target), a stash id, a `(selector, commit)` pair
/// — compared whole, because a yes addressed to yesterday's spelling of a
/// row is exactly the accident the double press exists to prevent.
///
/// When an arm dies is the pane's policy — a cursor move, a wheel, a
/// refresh, a prompt opening over it — and stays in the client. What the
/// slot *does* is here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Armed<T> {
    target: Option<T>,
}

impl<T> Default for Armed<T> {
    /// Nothing armed — how every pane opens.
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Armed<T> {
    /// Nothing armed — how every pane opens.
    pub fn new() -> Self {
        Self { target: None }
    }

    /// The target the question stands over, when one does — what the tests
    /// read to prove an arm moved, died, or stayed where it was asked.
    pub fn get(&self) -> Option<&T> {
        self.target.as_ref()
    }

    /// Whether a question is waiting for its second press.
    pub fn is_armed(&self) -> bool {
        self.target.is_some()
    }

    /// Arms `target` unconditionally — a set, not the dance: the worktree
    /// pane's force upgrade stands on the row a just-spent arm was over,
    /// set rather than re-earned.
    pub fn arm(&mut self, target: T) {
        self.target = Some(target);
    }

    /// Drops the question, whatever it was about — what `esc`, a prompt
    /// opening over the pane, a cursor move and every refresh reach.
    /// Idempotent.
    pub fn disarm(&mut self) {
        self.target = None;
    }

    /// Keeps the arm only while `still` says it means something — the
    /// conditional survival a refresh owes a queued upgrade: a force stands
    /// while its row does, and a removal that landed drops both.
    pub fn keep_if(&mut self, still: impl Fn(&T) -> bool) {
        self.target = self.target.take().filter(|t| still(t));
    }

    /// The position of the row the arm still stands over: the first row
    /// `same` names the armed target. Found per frame — the paint's tint is
    /// a property of the question, not of the draw. `None` while nothing is
    /// armed, and `None` when the row left the list under it.
    pub fn position_in<'a, R: 'a>(
        &self,
        rows: impl IntoIterator<Item = &'a R>,
        same: impl Fn(&R, &T) -> bool,
    ) -> Option<usize> {
        let target = self.target.as_ref()?;
        rows.into_iter().position(|r| same(r, target))
    }
}

impl<T: PartialEq> Armed<T> {
    /// Arms — or spends — the question on `target`. The first call on a
    /// target stores it and returns `false`: ask, don't act. A second call
    /// on the *same* target clears the arm and returns `true`: act. Anything
    /// else re-arms onto the new target and returns `false` again, so there
    /// is no state a caller has to remember.
    pub fn confirm_or_arm(&mut self, target: T) -> bool {
        let already = self.is(&target);
        self.target = match already {
            true => None,
            false => Some(target),
        };
        already
    }

    /// Whether the question stands over `target` — the paint's per-row tint
    /// check.
    pub fn is(&self, target: &T) -> bool {
        self.target.as_ref() == Some(target)
    }

    /// Drops the question unless it still stands over `at` — what a mouse
    /// press or a drag ending on a row owes the arm: attention moved, so a
    /// question about anywhere else dies; `None` — attention on furniture —
    /// drops it too.
    pub fn disarm_unless(&mut self, at: Option<&T>) {
        if self.target.as_ref() != at {
            self.target = None;
        }
    }
}

// ---------------------------------------------------------- the title strip

/// A pane's title-strip line: the repository's describe first, then each
/// piece — a count, a state — joined by ` · `, the separator the design
/// spells everywhere a label compounds.
///
/// `"{describe} · {n} changed"`, `"{describe} · {n} local · {m} remote"`,
/// `"{describe} · status unavailable"` are the one grammar; this is it.
pub fn label<I, P>(describe: &str, parts: I) -> String
where
    I: IntoIterator<Item = P>,
    P: AsRef<str>,
{
    let mut out = describe.to_string();
    for part in parts {
        out.push_str(" · ");
        out.push_str(part.as_ref());
    }
    out
}

/// `{n} {word}` with the one-word plural the labels spell — `1 tag`,
/// `3 tags`, `1 entry`, `2 entries`. A count of one reads its singular:
/// the grammar git's own config names use, not a formatting mood.
pub fn counted(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

/// The sentence a two-press verb asks through the status line: what the
/// second press spends, ending in the instruction — `"delete branch
/// feature? press again to confirm"`. One wording for every armed verb, so
/// the question reads the same in every pane and at every door — see
/// [`Armed`].
pub fn question(what: &str) -> String {
    format!("{what}? press again to confirm")
}

// --------------------------------------------------------------- the letters

/// What a status letter means, once you get past which side of the index it
/// is about — which is what decides its colour, and nothing else.
///
/// The same mapping both files panes made; the ink is a client's to choose —
/// the terminal colours by mark, the window by section — so `Mark` stops at
/// the letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Add,
    Modify,
    Delete,
    Rename,
    TypeChange,
    Untracked,
    Conflict,
}

impl Mark {
    /// From git's change letter set. A rename and a copy both mean "the
    /// index matched content across two paths", and draw alike.
    pub fn of(change: Change) -> Self {
        match change {
            Change::Added => Mark::Add,
            Change::Modified => Mark::Modify,
            Change::Deleted => Mark::Delete,
            Change::Renamed | Change::Copied => Mark::Rename,
            Change::TypeChanged => Mark::TypeChange,
        }
    }

    /// The single letter git prints.
    pub fn letter(self) -> &'static str {
        match self {
            Mark::Add => "A",
            Mark::Modify => "M",
            Mark::Delete => "D",
            Mark::Rename => "R",
            Mark::TypeChange => "T",
            // Known to no part of git: git itself prints `??`, and one
            // honest glyph beats two.
            Mark::Untracked => "?",
            Mark::Conflict => "",
        }
    }
}

/// The two-letter state of a conflicted path, exactly as porcelain v2 spells
/// it — who added and who deleted decides what resolving means, so the
/// letters are data and not decoration.
pub fn conflict_letters(state: ConflictKind) -> &'static str {
    match state {
        ConflictKind::BothDeleted => "DD",
        ConflictKind::AddedByUs => "AU",
        ConflictKind::DeletedByThem => "UD",
        ConflictKind::AddedByThem => "UA",
        ConflictKind::DeletedByUs => "DU",
        ConflictKind::BothAdded => "AA",
        ConflictKind::BothModified => "UU",
    }
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Viewport;

    /// A section type the tests can read.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sect {
        Local,
        Remote,
    }

    /// The sectioned fixture the tests draw from: `LOCAL / a / b /
    /// REMOTE / c` — positions 0..5, headings at 0 and 3.
    fn list() -> Vec<Row<Sect, &'static str>> {
        flatten([
            (Sect::Local, vec!["alpha", "beta"]),
            (Sect::Remote, vec!["origin/gamma"]),
        ])
    }

    /// What a query sees of the fixture's rows: an entry's text, nothing of
    /// a heading's — a `fn` because [`search_rows`] asks for one.
    fn entry_text<'a>(r: &'a Row<Sect, &'static str>) -> Option<&'a str> {
        r.entry().copied()
    }

    #[test]
    fn an_empty_section_earns_no_heading() {
        let rows = flatten([
            (Sect::Local, Vec::<&str>::new()),
            (Sect::Remote, vec!["origin/gamma"]),
        ]);
        assert_eq!(
            rows,
            [
                Row::Heading {
                    section: Sect::Remote,
                    count: "1".to_string(),
                },
                Row::Entry("origin/gamma"),
            ]
        );
        // All empty is an empty list, not a row of labels.
        assert!(flatten::<Sect, &str>([]).is_empty());
        assert!(
            flatten::<Sect, &str>([(Sect::Local, Vec::new()), (Sect::Remote, Vec::new())])
                .is_empty()
        );
    }

    #[test]
    fn flatten_orders_heading_then_entries_and_spells_the_count() {
        let rows = list();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].heading(), Some((&Sect::Local, "2")));
        assert_eq!(rows[3].heading(), Some((&Sect::Remote, "1")));
        assert_eq!(rows[1].entry(), Some(&"alpha"));
        assert_eq!(rows[4].entry(), Some(&"origin/gamma"));
    }

    #[test]
    fn the_row_halves_know_themselves() {
        let mut rows = list();
        assert!(rows[0].is_heading());
        assert!(!rows[1].is_heading());
        assert_eq!(rows[0].entry(), None);
        assert_eq!(rows[4].heading(), None);
        if let Some(e) = rows[2].entry_mut() {
            *e = "BETA";
        }
        assert_eq!(rows[2].entry(), Some(&"BETA"));
    }

    #[test]
    fn entries_walks_the_entry_half_only() {
        let rows = list();
        assert_eq!(
            entries(&rows).copied().collect::<Vec<_>>(),
            ["alpha", "beta", "origin/gamma"]
        );
        assert_eq!(entry_at(&rows, 0), None, "a heading is no entry");
        assert_eq!(entry_at(&rows, 9), None, "out of range is none");
        assert_eq!(entry_at(&rows, 2), Some(&"beta"));
    }

    #[test]
    fn the_numbering_skips_headings() {
        struct E {
            n: usize,
        }
        let mut rows: Vec<Row<Sect, E>> = flatten([
            (Sect::Local, vec![E { n: 0 }, E { n: 0 }]),
            (Sect::Remote, vec![E { n: 0 }]),
        ]);
        number(&mut rows, |e, n| e.n = n);
        let ns: Vec<usize> = entries(&rows).map(|e| e.n).collect();
        assert_eq!(ns, [1, 2, 3]);
        // The mutable walk writes the same half.
        for e in entries_mut(&mut rows) {
            e.n += 10;
        }
        assert_eq!(
            entries(&rows).map(|e| e.n).collect::<Vec<_>>(),
            [11, 12, 13]
        );
    }

    #[test]
    fn shown_names_the_source_row_through_the_table() {
        let rows = list();
        // A filter kept only the remote half: shown 0 is source 3.
        let visible = vec![3, 4];
        assert_eq!(shown(&rows, &visible, 0), rows.get(3));
        assert_eq!(entry_at_shown(&rows, &visible, 1), Some(&"origin/gamma"));
        assert_eq!(
            entry_at_shown(&rows, &visible, 0),
            None,
            "a heading is no entry"
        );
        assert_eq!(shown(&rows, &visible, 2), None);
    }

    #[test]
    fn the_cursor_never_rests_on_a_heading() {
        let rows = list();
        let mut v = Viewport::new();
        v.set_len(rows.len());
        // Landing on the LOCAL heading steps on in the direction of travel.
        v.go_to(0);
        v.settle(0, |i| selectable(&rows, Row::is_heading, i));
        assert_eq!(v.cursor(), 1);
        // `k` back onto it turns around at the edge instead of sticking.
        v.go_to(0);
        v.settle(2, |i| selectable(&rows, Row::is_heading, i));
        assert_eq!(v.cursor(), 1);
        // Under a filter the same rule runs in shown space.
        let visible = search_rows(&rows, "gamma", Row::is_heading, entry_text);
        assert_eq!(visible, [3, 4]);
        v.set_len(visible.len());
        v.go_to(0);
        v.settle(0, |i| selectable_shown(&rows, &visible, Row::is_heading, i));
        assert_eq!(v.cursor(), 1);
    }

    #[test]
    fn settle_at_walks_off_furniture_in_the_direction_of_the_move() {
        let rows = list();
        assert_eq!(settle_at(&rows, Row::is_heading, 0, 1), 1);
        // Upward off the REMOTE heading finds beta, not the heading's edge.
        assert_eq!(settle_at(&rows, Row::is_heading, 3, -1), 2);
        // Downward off it finds its own first entry.
        assert_eq!(settle_at(&rows, Row::is_heading, 3, 1), 4);
        // An entry settles where it is; out of range answers `at`.
        assert_eq!(settle_at(&rows, Row::is_heading, 2, 1), 2);
        assert_eq!(settle_at(&rows, Row::is_heading, 9, 1), 9);
        // Nothing but furniture leaves the cursor where it was — the one
        // honest answer when no row qualifies.
        let furniture: Vec<Row<Sect, &str>> = vec![Row::Heading {
            section: Sect::Local,
            count: "0".into(),
        }];
        assert_eq!(settle_at(&furniture, Row::is_heading, 0, 1), 0);
        assert!(settle_at(&furniture, Row::is_heading, 0, -1) == 0);
    }

    #[test]
    fn settle_shown_judges_the_row_the_position_names() {
        let rows = list();
        let visible = vec![0usize, 2, 4];
        // Shown 0 is the LOCAL heading, whose group lost a row to the
        // filter but not all of them.
        assert_eq!(settle_shown(&rows, &visible, Row::is_heading, 0, 1), 1);
        assert_eq!(settle_shown(&rows, &visible, Row::is_heading, 0, -1), 1);
        assert_eq!(settle_shown(&rows, &visible, Row::is_heading, 2, -1), 2);
    }

    #[test]
    fn a_match_jump_wraps_where_a_cursor_move_clamps() {
        assert_eq!(wrap_index(2, 1, 3), 0);
        assert_eq!(wrap_index(0, -1, 3), 2);
        assert_eq!(wrap_index(1, 1, 3), 2);
        assert_eq!(wrap_index(4, 0, 7), 4);
        assert_eq!(wrap_index(4, 1, 0), 4, "an empty list goes nowhere");
    }

    #[test]
    fn matches_folds_both_sides() {
        assert!(matches("src/main.rs", "MAIN"));
        assert!(matches("README", "read"));
        assert!(!matches("alpha", "beta"));
        assert!(matches("anything", ""));
        assert!(matches("ÜNICODE", "ünicode"));
    }

    #[test]
    fn a_query_keeps_the_heading_while_it_keeps_a_row() {
        let rows = list();
        // One hit under LOCAL keeps the heading once.
        assert_eq!(
            search_rows(&rows, "alpha", Row::is_heading, entry_text),
            [0, 1]
        );
        // A hit under REMOTE drops LOCAL's heading with its group.
        assert_eq!(
            search_rows(&rows, "gamma", Row::is_heading, entry_text),
            [3, 4]
        );
        // Hits under both keep both.
        assert_eq!(
            search_rows(&rows, "a", Row::is_heading, entry_text),
            [0, 1, 2, 3, 4]
        );
        // Nothing matched: no orphan headings.
        assert_eq!(
            search_rows(&rows, "zzz", Row::is_heading, entry_text),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn an_empty_query_is_every_row_and_a_padded_one_is_trimmed() {
        let rows = list();
        let all = identity(&rows);
        assert_eq!(all, [0, 1, 2, 3, 4]);
        assert_eq!(search_rows(&rows, "", Row::is_heading, entry_text), all);
        assert_eq!(search_rows(&rows, "   ", Row::is_heading, entry_text), all);
        assert_eq!(
            search_rows(&rows, "  beta ", Row::is_heading, entry_text),
            [0, 2]
        );
    }

    #[test]
    fn a_row_no_query_names_shows_unfiltered() {
        // A detached-HEAD row ahead of the sections: `text` says nothing
        // about it, so a real query drops it like the branches pane's.
        fn detached_or_entry<'a>(r: &'a Row<Sect, &'static str>) -> Option<&'a str> {
            match r {
                Row::Entry("* detached") => None,
                Row::Entry(e) => Some(*e),
                Row::Heading { .. } => None,
            }
        }
        let mut rows = vec![Row::<Sect, &str>::Entry("* detached")];
        rows.extend(list());
        assert_eq!(
            search_rows(&rows, "", Row::is_heading, detached_or_entry).len(),
            6
        );
        assert_eq!(
            search_rows(&rows, "gamma", Row::is_heading, detached_or_entry),
            [4, 5]
        );
    }

    #[test]
    fn a_flat_list_searches_without_headings() {
        // The stashes shape: no section type at all, just rows.
        fn whole<'a>(r: &'a &'static str) -> Option<&'a str> {
            Some(*r)
        }
        let rows = ["stash@{0}: wip", "stash@{1}: fix tests"];
        assert_eq!(search_rows(&rows, "fix", |_| false, whole), [1]);
        assert_eq!(search_rows(&rows, "", |_| false, whole), [0, 1]);
    }

    #[test]
    fn normalize_query_treats_an_empty_prompt_as_no_filter() {
        assert_eq!(normalize_query("  feature "), Some("feature".to_string()));
        assert_eq!(normalize_query(""), None);
        assert_eq!(normalize_query("   "), None);
    }

    #[test]
    fn filter_note_spells_shown_over_total_only_while_filtered() {
        assert_eq!(filter_note(Some("alp"), 3, 15), Some("3/15".to_string()));
        assert_eq!(filter_note(None, 15, 15), None);
    }

    #[test]
    fn the_first_press_arms_and_the_second_on_the_same_row_acts() {
        let mut armed: Armed<(Sect, String)> = Armed::new();
        assert!(!armed.is_armed());
        assert_eq!(armed.get(), None);
        assert!(!armed.confirm_or_arm((Sect::Local, "a.rs".to_string())));
        assert!(armed.is_armed());
        assert_eq!(armed.get(), Some(&(Sect::Local, "a.rs".to_string())));
        // A second press on the same target spends the arm and runs.
        assert!(armed.confirm_or_arm((Sect::Local, "a.rs".to_string())));
        assert_eq!(armed.get(), None);
    }

    #[test]
    fn a_different_target_moves_the_question_rather_than_queueing_two() {
        let mut armed = Armed::new();
        assert!(!armed.confirm_or_arm("a.rs"));
        assert!(
            !armed.confirm_or_arm("b.rs"),
            "the new target arms, not acts"
        );
        assert_eq!(armed.get(), Some(&"b.rs"));
        assert!(armed.confirm_or_arm("b.rs"));
    }

    #[test]
    fn attention_moving_off_the_row_drops_the_question() {
        let mut armed = Armed::new();
        armed.arm(7usize);
        armed.disarm_unless(Some(&7));
        assert!(armed.is_armed(), "same row keeps the question");
        armed.disarm_unless(Some(&8));
        assert_eq!(armed.get(), None, "another row drops it");
        armed.arm(7);
        armed.disarm_unless(None);
        assert_eq!(armed.get(), None, "attention on furniture drops it");
        // Idempotent: disarming nothing is still nothing.
        armed.disarm();
        armed.disarm();
        assert!(!armed.is_armed());
    }

    #[test]
    fn keep_if_holds_an_arm_while_its_condition_does() {
        // The worktree pane's force upgrade: the arm survives a refresh
        // while its path does, and a removal that landed drops both.
        let mut armed = Armed::new();
        armed.arm(("/tmp/wt".to_string(), true));
        armed.keep_if(|(path, force)| *force && path == "/tmp/wt");
        assert!(armed.is_armed());
        armed.keep_if(|(_, force)| !force);
        assert_eq!(armed.get(), None);
        // And `arm` alone sets without earning a first press — the upgrade.
        armed.arm(("/tmp/wt".to_string(), true));
        assert!(armed.is(&("/tmp/wt".to_string(), true)));
        assert!(!armed.is(&("/tmp/wt".to_string(), false)));
    }

    #[test]
    fn position_in_finds_the_row_the_question_stands_over() {
        let rows = ["a.rs", "b.rs", "c.rs"];
        let mut armed: Armed<String> = Armed::new();
        assert_eq!(armed.position_in(&rows, |r, t| *r == t), None);
        armed.arm("b.rs".to_string());
        assert_eq!(armed.position_in(&rows, |r, t| *r == t.as_str()), Some(1));
        // The row leaving the list under the arm answers None — the paint
        // tints nothing it cannot find.
        armed.arm("gone.rs".to_string());
        assert_eq!(armed.position_in(&rows, |r, t| *r == t.as_str()), None);
        // Through the shown table is the same call, rows mapped first.
        let visible = [2usize, 1];
        assert_eq!(
            armed.position_in(visible.iter().map(|&d| &rows[d]), |r, t| {
                *r == t.as_str()
            }),
            None
        );
        armed.arm("a.rs".to_string());
        assert_eq!(
            armed.position_in(visible.iter().map(|&d| &rows[d]), |r, t| {
                *r == t.as_str()
            }),
            None,
            "a.rs is not in the shown table"
        );
        armed.arm("c.rs".to_string());
        assert_eq!(
            armed.position_in(visible.iter().map(|&d| &rows[d]), |r, t| {
                *r == t.as_str()
            }),
            Some(0)
        );
    }

    #[test]
    fn the_title_strip_joins_its_pieces() {
        assert_eq!(
            label("gitten (main)", ["3 changed"]),
            "gitten (main) · 3 changed"
        );
        assert_eq!(
            label("gitten (main)", ["2 local", "1 remote"]),
            "gitten (main) · 2 local · 1 remote"
        );
        assert_eq!(
            label("gitten", ["status unavailable".to_string()]),
            "gitten · status unavailable"
        );
        assert_eq!(label("gitten", Vec::<String>::new()), "gitten");
    }

    #[test]
    fn a_count_takes_the_plural() {
        assert_eq!(counted(0, "tag", "tags"), "0 tags");
        assert_eq!(counted(1, "tag", "tags"), "1 tag");
        assert_eq!(counted(2, "entry", "entries"), "2 entries");
        assert_eq!(counted(1, "entry", "entries"), "1 entry");
    }

    #[test]
    fn the_question_ends_in_the_instruction() {
        assert_eq!(
            question("delete branch feature"),
            "delete branch feature? press again to confirm"
        );
        assert_eq!(
            question("discard changes to src/main.rs"),
            "discard changes to src/main.rs? press again to confirm"
        );
    }

    #[test]
    fn the_marks_are_what_the_letter_means() {
        assert_eq!(Mark::of(Change::Added), Mark::Add);
        assert_eq!(Mark::of(Change::Modified), Mark::Modify);
        assert_eq!(Mark::of(Change::Deleted), Mark::Delete);
        // A rename and a copy draw alike — the index matched content.
        assert_eq!(Mark::of(Change::Renamed), Mark::Rename);
        assert_eq!(Mark::of(Change::Copied), Mark::Rename);
        assert_eq!(Mark::of(Change::TypeChanged), Mark::TypeChange);
        assert_eq!(Mark::Add.letter(), "A");
        assert_eq!(Mark::Modify.letter(), "M");
        assert_eq!(Mark::Delete.letter(), "D");
        assert_eq!(Mark::Rename.letter(), "R");
        assert_eq!(Mark::TypeChange.letter(), "T");
        assert_eq!(Mark::Untracked.letter(), "?");
        assert_eq!(Mark::Conflict.letter(), "");
    }

    #[test]
    fn the_conflict_letters_are_porcelains_own() {
        assert_eq!(conflict_letters(ConflictKind::BothDeleted), "DD");
        assert_eq!(conflict_letters(ConflictKind::AddedByUs), "AU");
        assert_eq!(conflict_letters(ConflictKind::DeletedByThem), "UD");
        assert_eq!(conflict_letters(ConflictKind::AddedByThem), "UA");
        assert_eq!(conflict_letters(ConflictKind::DeletedByUs), "DU");
        assert_eq!(conflict_letters(ConflictKind::BothAdded), "AA");
        assert_eq!(conflict_letters(ConflictKind::BothModified), "UU");
    }

    #[test]
    fn count_distinct_counts_a_path_in_two_sections_once() {
        let rows = flatten([
            (Sect::Local, vec!["src/a.rs", "src/b.rs"]),
            (Sect::Remote, vec!["src/a.rs"]),
        ]);
        assert_eq!(count_distinct(&rows, |p| *p), 2);
    }
}
