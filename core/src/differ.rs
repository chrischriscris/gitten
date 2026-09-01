//! Which algorithm turned two files into a diff.
//!
//! Everything else in the pipeline starts from a `FileDiff`. This is the stage
//! before that: two lists of lines in, an edit script out, hunks assembled from
//! it. It is a seam rather than a function because "which lines correspond" is a
//! judgement call, not a fact — Myers minimises the edit count, Histogram
//! anchors on rare lines, and a semantic differ would ignore both and compare
//! syntax trees.
//!
//! # Why this is in `core` and not behind a crate
//!
//! `core` has no dependencies and that is not negotiable, so the algorithms are
//! written here rather than pulled in. Two of the three are textbook and the
//! third is a parameter change; the whole module is smaller than the wrapper
//! around a third-party differ would have been, and it keeps `cargo test -p
//! gitten-core` at well under a second.
//!
//! # The split of responsibility
//!
//! An implementation produces only the [`Edit`] script. Line numbers, context
//! lines, hunk headers and the `@@ ... @@ fn name` suffix are [`hunks`], shared
//! by every differ — that bookkeeping is identical for all of them and getting
//! it subtly wrong in a second place is exactly the kind of bug nobody sees
//! until a hunk header points at the wrong line.
//!
//! ```ignore
//! host.differ.register(TreeSitterDiff::new());     // a new algorithm
//! host.differ.select("tree-sitter");               // for everything
//! host.differ.route(&["json", "lock"], "myers");   // or just for some paths
//! ```
//!
//! # The guards, and why they are here
//!
//! Both algorithms are worst-case quadratic in the number of *differing* lines,
//! and real repositories contain 700k-line machine-generated diffs. Every
//! recursion is bounded by [`MAX_STEPS`] and degrades to a whole-region replace
//! rather than stalling the load — the same trade as `MAX_INTRALINE_TOKENS`, for
//! the same reason: a line-for-line pairing of a generated file is not worth a
//! visible pause, and nobody was reading it anyway.
//!
//! Recursion is an explicit stack, not the call stack. A 50k-line file whose
//! every anchor peels off one line is a 50k-deep recursion, which is a stack
//! overflow rather than a slow load, and it is not a hypothetical shape — a
//! generated file with a repeating structure produces exactly it.
//!
//! # The cache
//!
//! Assembled hunks are remembered against both sides' blob OIDs plus every
//! setting that reaches the answer — resolved algorithm, whitespace relation,
//! context, move floor, indent heuristic ([`Cache`]). A blob never changes,
//! so equal OIDs mean an equal answer, which is why the key can be that small:
//! the cache changes what a diff costs and never what it says. A side with no
//! OID — untracked, added or deleted, a gitlink — has no identity to remember
//! it by and always computes. Nothing here makes a cold diff cheaper: the first
//! acquisition of anything, every file that actually changed, and every `git`
//! spawn stay full-price — the cache only stops unchanged files paying twice.

use crate::{DiffLine, FileDiff, Hunk, LineKind};
use std::collections::VecDeque;
use std::hash::Hasher;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

// ------------------------------------------------------------- the edit script

/// Replace `old[old_start..old_end]` with `new[new_start..new_end]`.
///
/// An empty old range is a pure insertion, an empty new range a pure deletion,
/// and both empty never happens. The contract an implementation owes: sorted by
/// `old_start`, non-overlapping, and no two adjacent — `verify` in the tests
/// checks all of it, and every built-in is run through it.
///
/// `u32` because a diff of more than four billion lines is not a diff, and 16
/// bytes per edit keeps a script of a large file in cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    pub old_start: u32,
    pub old_end: u32,
    pub new_start: u32,
    pub new_end: u32,
}

impl Edit {
    pub fn old_range(&self) -> std::ops::Range<usize> {
        self.old_start as usize..self.old_end as usize
    }

    pub fn new_range(&self) -> std::ops::Range<usize> {
        self.new_start as usize..self.new_end as usize
    }

    pub fn is_empty(&self) -> bool {
        self.old_start == self.old_end && self.new_start == self.new_end
    }
}

/// Two files in, the edits between them out.
///
/// Lines rather than bytes because that is the unit a diff view addresses, and
/// `path` because a language-aware implementation needs to know what it is
/// looking at — the same argument as
/// [`Highlighter::highlight`](crate::syntax::Highlighter::highlight).
///
/// The lines arrive as shared handles, which an implementation is free to keep:
/// equality and hashing go through the text, so a handle compares as its content.
///
/// The name is what a config file and a keybinding refer to, which is why it is
/// `&'static str` and part of the trait rather than a wrapper's business.
/// Implementations are thread-safe so a configured registry can be cloned into
/// a pane's background refresh rather than rebuilt from built-ins there.
pub trait Differ: Send + Sync {
    fn name(&self) -> &'static str;

    fn diff(&self, path: &str, old: &[Arc<str>], new: &[Arc<str>]) -> Vec<Edit>;

    /// The same answer as [`Differ::diff`], for a caller that has already
    /// interned the keys. `ids` are dense from zero and equal exactly when the
    /// keys are, so an implementation whose inner loops compare lines can skip a
    /// hash pass the caller has already paid for.
    ///
    /// The default ignores them, which is the right answer for an implementation
    /// that needs the text itself — a semantic differ reads words, not numbers.
    /// Overriding this is an optimisation and never a change of answer: both
    /// methods must return the same edit script for the same input.
    fn diff_interned(
        &self,
        path: &str,
        old: &[Arc<str>],
        new: &[Arc<str>],
        ids: (&[u32], &[u32]),
    ) -> Vec<Edit> {
        let _ = ids;
        self.diff(path, old, new)
    }
}

/// Every line replaced by a number, so the inner loops compare `u32`s.
///
/// Public because an implementation will want it: string comparison inside an
/// O(ND) loop is most of the runtime, and interning is the whole reason the
/// textbook algorithms are fast enough to be shipped as written.
pub fn intern(old: &[Arc<str>], new: &[Arc<str>]) -> (Vec<u32>, Vec<u32>) {
    // One map over both sides, so a line present in each gets one id.
    let mut map: LineMap = LineMap::with_capacity_and_hasher(old.len() + new.len(), <_>::default());
    let mut a = Vec::with_capacity(old.len());
    let mut b = Vec::with_capacity(new.len());
    number(&mut map, old, &mut a);
    number(&mut map, new, &mut b);
    (a, b)
}

/// Line text to id, on [`crate::FxHasher`].
///
/// **The hottest map in the application**, and it was the last one still on
/// SipHash — the author map in `parse_log` moved years of measurement ago and
/// this did not, which cost *half the differ's runtime*: 52.1 ms → 24.3 ms on
/// `md.diff`, 45.7 → 20.9 on `pr30683`, with `diffcheck` reporting the same
/// changed-line counts and the same hunk positions as git before and after. The
/// alias is what stops the two drifting apart again.
type LineMap<'a> = crate::FxHashMap<&'a Arc<str>, u32>;

/// Numbers `lines` into `out` through `map`. Keys borrow the caller's handles,
/// so nothing is copied — only the pointed-at text is hashed.
fn number<'a>(map: &mut LineMap<'a>, lines: &'a [Arc<str>], out: &mut Vec<u32>) {
    out.clear();
    out.reserve(lines.len());
    for line in lines {
        let next = map.len() as u32;
        out.push(*map.entry(line).or_insert(next));
    }
}

/// How many diagonal steps a single file's diff may spend before it degrades to
/// a whole-region replace.
///
/// Myers is O((N+M)D) and Histogram falls back to it on any region with no rare
/// line to anchor on, so a fully rewritten generated file is the case that
/// matters: 50k lines against 50k different lines is 10^10 steps, which is
/// minutes. 40 million is tens of milliseconds, and past it the honest answer is
/// "this file was replaced" — which is what the diff would have looked like
/// anyway.
pub const MAX_STEPS: usize = 40_000_000;

/// How often a line may appear in a region and still be usable as an anchor.
///
/// Histogram's whole idea: a line that appears once is almost certainly the same
/// line, and `}` appearing four hundred times tells you nothing. Git's xdiff
/// uses 64 for the same purpose and the same reason, and matching it means a
/// diff of the same file looks the same in both tools.
pub const MAX_ANCHOR_OCCURRENCES: u32 = 64;

// -------------------------------------------------------------- the built-ins

/// The default. Anchors each region on its rarest common line, recurses either
/// side of it, and falls back to [`Myers`] for a region with nothing rare in it.
///
/// Why this and not Myers, which git defaults to: a minimal edit script is not
/// the same thing as a readable one. Myers is free to match any `}` to any other
/// `}`, so a block moved down a file dissolves into a mesh of one-line changes.
/// Anchoring on lines that appear once means a function signature becomes the
/// anchor and the moved block reads as a move — see
/// `docs/decisions/0001-histogram-not-myers.md`.
#[derive(Debug, Default)]
pub struct Histogram {
    scratch: Mutex<Ctx>,
}

/// Anchors only on lines appearing *exactly* once, and falls back to [`Myers`]
/// everywhere else.
///
/// [`Histogram`] with the rarity threshold at one, which is patience diff's
/// idea. Not its implementation: git's `--patience` takes the longest increasing
/// subsequence of *all* unique-line matches at once, where this takes the best
/// single anchor and recurses. The two agree on almost everything — measured
/// over this repository's whole history they differ by 4 changed lines in 9,958,
/// and `git/examples/diffcheck.rs` is what reports that.
///
/// Worth its own name because the difference from Histogram is visible on real
/// code: a file with three identical `impl Default for` blocks gets anchored by
/// Histogram and not by this, and which reads better depends on the file.
#[derive(Debug, Default)]
pub struct Patience {
    scratch: Mutex<Ctx>,
}

/// The minimal edit script, by Myers' 1986 algorithm with the linear-space
/// middle-snake refinement.
///
/// Fewest added and removed lines of any possible diff, which is what `git diff`
/// produces by default. Available because "smallest" is sometimes exactly what
/// is wanted — reviewing a whitespace change, or checking that a refactor really
/// did not touch anything else.
#[derive(Debug, Default)]
pub struct Myers {
    scratch: Mutex<Ctx>,
}

impl Differ for Histogram {
    fn name(&self) -> &'static str {
        "histogram"
    }

    fn diff(&self, _path: &str, old: &[Arc<str>], new: &[Arc<str>]) -> Vec<Edit> {
        self.scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diffed(old, new, Some(MAX_ANCHOR_OCCURRENCES))
    }

    fn diff_interned(
        &self,
        _path: &str,
        _old: &[Arc<str>],
        _new: &[Arc<str>],
        ids: (&[u32], &[u32]),
    ) -> Vec<Edit> {
        self.scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diffed_ids(ids.0, ids.1, Some(MAX_ANCHOR_OCCURRENCES))
    }
}

impl Differ for Patience {
    fn name(&self) -> &'static str {
        "patience"
    }

    fn diff(&self, _path: &str, old: &[Arc<str>], new: &[Arc<str>]) -> Vec<Edit> {
        self.scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diffed(old, new, Some(1))
    }

    fn diff_interned(
        &self,
        _path: &str,
        _old: &[Arc<str>],
        _new: &[Arc<str>],
        ids: (&[u32], &[u32]),
    ) -> Vec<Edit> {
        self.scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diffed_ids(ids.0, ids.1, Some(1))
    }
}

impl Differ for Myers {
    fn name(&self) -> &'static str {
        "myers"
    }

    fn diff(&self, _path: &str, old: &[Arc<str>], new: &[Arc<str>]) -> Vec<Edit> {
        self.scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diffed(old, new, None)
    }

    fn diff_interned(
        &self,
        _path: &str,
        _old: &[Arc<str>],
        _new: &[Arc<str>],
        ids: (&[u32], &[u32]),
    ) -> Vec<Edit> {
        self.scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diffed_ids(ids.0, ids.1, None)
    }
}

// ----------------------------------------------------------------- the region

/// A rectangle of the two files still to be diffed. Index ranges rather than
/// subslices so an explicit work stack can hold them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    a0: usize,
    a1: usize,
    b0: usize,
    b1: usize,
}

impl Region {
    fn whole(a: &[u32], b: &[u32]) -> Self {
        Self {
            a0: 0,
            a1: a.len(),
            b0: 0,
            b1: b.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.a0 == self.a1 && self.b0 == self.b1
    }

    fn edit(&self) -> Edit {
        Edit {
            old_start: self.a0 as u32,
            old_end: self.a1 as u32,
            new_start: self.b0 as u32,
            new_end: self.b1 as u32,
        }
    }

    /// Peels the lines that already match off both ends. Every algorithm here
    /// wants this first, and after it `a[a0] != b[b0]`, which is what stops the
    /// recursion below from splitting a region into itself.
    fn trimmed(mut self, a: &[u32], b: &[u32]) -> Self {
        while self.a0 < self.a1 && self.b0 < self.b1 && a[self.a0] == b[self.b0] {
            self.a0 += 1;
            self.b0 += 1;
        }
        while self.a0 < self.a1 && self.b0 < self.b1 && a[self.a1 - 1] == b[self.b1 - 1] {
            self.a1 -= 1;
            self.b1 -= 1;
        }
        self
    }
}

/// Merges into the previous edit when they touch, so a script never contains two
/// adjacent edits describing one change. The fallback paths can produce them.
fn push(out: &mut Vec<Edit>, e: Edit) {
    if e.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.old_end == e.old_start && last.new_end == e.new_start => {
            last.old_end = e.old_end;
            last.new_end = e.new_end;
        }
        _ => out.push(e),
    }
}

// ------------------------------------------------------------ the shared state

/// Scratch space one built-in differ reuses for its whole life: grown once and
/// cleared per region and per file, never reallocated. Held behind a `RefCell`
/// by [`Histogram`], [`Patience`] and [`Myers`], because `Differ::diff` takes
/// `&self` — the registry is shared and immutable.
#[derive(Debug, Default)]
struct Ctx {
    steps: usize,
    /// Rebuilt per region rather than per file: rarity is only meaningful
    /// relative to the region being anchored, so a `}` that appears twice in the
    /// six lines under consideration is a usable anchor even though it appears
    /// four hundred times in the file. Git's histogram does the same — which is
    /// also why the hasher matters here: this is rebuilt for every region of
    /// every file, so it is hashed more often than anything else in the pipeline.
    occurrences: crate::FxHashMap<u32, Chain>,
    /// Myers' forward and backward frontiers, sized for the largest span seen
    /// and reused across every region of every file — including each
    /// TooCommon fallback an anchored search hands over.
    fwd: V,
    bwd: V,
    /// The interning output buffers. Taken out of here while the search borrows
    /// them and returned afterwards, so their capacity survives the borrow.
    ids_old: Vec<u32>,
    ids_new: Vec<u32>,
}

/// Where a line appears in a region, and how often.
///
/// The position list is capped because the anchor search walks it per candidate;
/// the count is not, because the count is what decides whether the line is worth
/// anchoring on at all.
#[derive(Debug, Default)]
struct Chain {
    count: u32,
    at: Vec<u32>,
}

impl Ctx {
    /// Per-file state: the budget and everything derived from one file's lines.
    /// Buffer capacity is deliberately left alone.
    fn begin_file(&mut self) {
        self.steps = MAX_STEPS;
        self.occurrences.clear();
    }

    /// One file, start to finish: intern both sides through the retained
    /// buffers, then run the search on them. `Some(max)` anchors with that
    /// rarity threshold, `None` runs plain Myers.
    fn diffed(&mut self, old: &[Arc<str>], new: &[Arc<str>], max: Option<u32>) -> Vec<Edit> {
        // One map over both sides; keys borrow the caller's handles and die with
        // this call, which is why the map cannot live in `Ctx`.
        //
        // Sized up front. It cannot keep its capacity across files the way the
        // buffers either side of it do — the lifetime is what forbids that — so
        // the one thing left to avoid is rehashing on the way up, which on a
        // 700k-line file is twenty reallocations of everything interned so far.
        let mut map: LineMap =
            LineMap::with_capacity_and_hasher(old.len() + new.len(), <_>::default());
        number(&mut map, old, &mut self.ids_old);
        let a = std::mem::take(&mut self.ids_old);
        number(&mut map, new, &mut self.ids_new);
        let b = std::mem::take(&mut self.ids_new);
        let out = self.diffed_ids(&a, &b, max);
        // Handed back rather than dropped: the next file starts at full size.
        self.ids_old = a;
        self.ids_new = b;
        out
    }

    /// The search itself, over ids the caller supplies. `diffed` is this with an
    /// interning pass in front of it.
    fn diffed_ids(&mut self, a: &[u32], b: &[u32], max: Option<u32>) -> Vec<Edit> {
        self.begin_file();
        let mut out = Vec::new();
        match max {
            Some(max) => self.anchored(a, b, max, &mut out),
            None => self.myers(a, b, Region::whole(a, b), &mut out),
        }
        out
    }

    /// Spends `n` steps, reporting whether the budget survived.
    #[inline]
    fn spend(&mut self, n: usize) -> bool {
        self.steps = self.steps.saturating_sub(n);
        self.steps > 0
    }

    // ------------------------------------------------------------------ myers

    /// Myers, as an explicit depth-first traversal so the emitted edits come out
    /// in increasing order: the left half is pushed last and therefore popped
    /// first.
    fn myers(&mut self, a: &[u32], b: &[u32], region: Region, out: &mut Vec<Edit>) {
        // The frontiers are sized for this span and kept afterwards — an
        // anchored file hands every TooCommon region here, and reallocating
        // per region is most of what a fallback-heavy diff costs.
        let span = (region.a1 - region.a0) + (region.b1 - region.b0);
        self.fwd.grow(span);
        self.bwd.grow(span);
        let mut stack = vec![region];
        while let Some(r) = stack.pop() {
            let r = r.trimmed(a, b);
            if r.is_empty() {
                continue;
            }
            // One side exhausted: the rest is a pure insertion or deletion and
            // there is nothing to search for.
            if r.a0 == r.a1 || r.b0 == r.b1 {
                push(out, r.edit());
                continue;
            }
            match self.split(a, b, r) {
                Some((x, y)) => {
                    stack.push(Region {
                        a0: x,
                        a1: r.a1,
                        b0: y,
                        b1: r.b1,
                    });
                    stack.push(Region {
                        a0: r.a0,
                        a1: x,
                        b0: r.b0,
                        b1: y,
                    });
                }
                // Out of budget, or a split that would have recursed on the
                // region it was given. Either way: this region was replaced.
                None => push(out, r.edit()),
            }
        }
    }

    /// One point on a minimal path through `region`, found by walking a forward
    /// and a backward frontier one edit-distance at a time until they overlap.
    ///
    /// Returns the meeting point rather than the whole middle snake. Splitting
    /// there and diffing both halves still yields a minimal script — the halves
    /// cost `d_forward + d_backward`, which is the distance the frontiers just
    /// proved — and it is half the bookkeeping.
    ///
    /// `None` means the budget ran out, or the meeting point was a corner of the
    /// region, which would have recursed on the region itself forever. A trimmed
    /// region should make the corner unreachable; this does not rely on that.
    fn split(&mut self, a: &[u32], b: &[u32], r: Region) -> Option<(usize, usize)> {
        let (a, b) = (&a[r.a0..r.a1], &b[r.b0..r.b1]);
        let (n, m) = (a.len(), b.len());
        let delta = n as isize - m as isize;
        let dmax = ((n + m).div_ceil(2) + 1) as isize;

        // Only `1` needs seeding: at every depth the recurrence reads diagonals
        // one *closer* to the centre than the ones it writes, never further, so
        // nothing stale is ever read and neither array needs clearing.
        self.fwd.set(1, 0);
        self.bwd.set(1, 0);

        for d in 0..=dmax {
            let mut k = d;
            while k >= -d {
                if !self.spend(1) {
                    return None;
                }
                let mut x = if k == -d || (k != d && self.fwd.get(k - 1) < self.fwd.get(k + 1)) {
                    self.fwd.get(k + 1)
                } else {
                    self.fwd.get(k - 1) + 1
                };
                let mut y = (x as isize - k) as usize;
                while x < n && y < m && a[x] == b[y] {
                    x += 1;
                    y += 1;
                }
                self.fwd.set(k, x);
                // The backward frontier is one depth behind, so only the
                // diagonals it has actually reached may be consulted. An odd
                // delta is when the two frontiers can meet on a forward step.
                if delta % 2 != 0
                    && d >= 1
                    && (delta - k).abs() < d
                    && x + self.bwd.get(delta - k) >= n
                {
                    return inside(r, x, y, n, m);
                }
                k -= 2;
            }

            let mut k = d;
            while k >= -d {
                if !self.spend(1) {
                    return None;
                }
                // The same recurrence in the frame where both files are
                // reversed, so `x` here is a distance from the end.
                let mut x = if k == -d || (k != d && self.bwd.get(k - 1) < self.bwd.get(k + 1)) {
                    self.bwd.get(k + 1)
                } else {
                    self.bwd.get(k - 1) + 1
                };
                let mut y = (x as isize - k) as usize;
                while x < n && y < m && a[n - x - 1] == b[m - y - 1] {
                    x += 1;
                    y += 1;
                }
                self.bwd.set(k, x);
                if delta % 2 == 0 && (delta - k).abs() <= d && self.fwd.get(delta - k) + x >= n {
                    return inside(r, n - x, m - y, n, m);
                }
                k -= 2;
            }
        }
        None
    }

    // --------------------------------------------------------------- anchored

    /// Histogram and patience: the same traversal, a different rarity threshold.
    fn anchored(&mut self, a: &[u32], b: &[u32], max: u32, out: &mut Vec<Edit>) {
        let mut stack = vec![Region::whole(a, b)];
        while let Some(r) = stack.pop() {
            let r = r.trimmed(a, b);
            if r.is_empty() {
                continue;
            }
            if r.a0 == r.a1 || r.b0 == r.b1 {
                push(out, r.edit());
                continue;
            }
            match self.anchor(a, b, r, max) {
                // Nothing in common at all: the region was rewritten, and no
                // algorithm has anything useful to say about it.
                Anchor::Disjoint => push(out, r.edit()),
                // Lines in common, but every one of them is too repetitive to
                // trust as an anchor — or the budget is gone. This is the case
                // histogram exists in order not to guess at, so hand it to the
                // algorithm that does not guess.
                Anchor::TooCommon => self.myers(a, b, r, out),
                Anchor::At { a_at, b_at, len } => {
                    stack.push(Region {
                        a0: a_at + len,
                        a1: r.a1,
                        b0: b_at + len,
                        b1: r.b1,
                    });
                    stack.push(Region {
                        a0: r.a0,
                        a1: a_at,
                        b0: r.b0,
                        b1: b_at,
                    });
                }
            }
        }
    }

    /// The best anchor in a region: the run containing the rarest line, or
    /// failing that the longest run.
    ///
    /// A run is scored by the *rarest* line in it, not the most common one. That
    /// distinction is the whole algorithm and it is easy to get backwards: score
    /// by the most common line and a four-hundred-line run of unique code loses
    /// to a one-line run somewhere else the moment a single `}` falls inside it,
    /// which fragments a clean diff into hundreds of one-line changes. Measured
    /// on this repository's own history the wrong way round cost 582 spurious
    /// changed-line pairs on a 690-line file — `git/examples/diffcheck.rs` is
    /// what catches it.
    ///
    /// Accepted if strictly rarer *or* strictly longer, which is git's rule
    /// rather than a lexicographic sort — a longer run wins even carrying a more
    /// common line, because length is evidence too.
    ///
    /// The index is rebuilt per region and charged to the step budget, because
    /// the recursion is only balanced when anchors are long: a file whose every
    /// anchor peels off one line is quadratic, and that shape is what generated
    /// code looks like. Past the budget it degrades to Myers rather than
    /// finishing eventually.
    fn anchor(&mut self, a: &[u32], b: &[u32], r: Region, max: u32) -> Anchor {
        if !self.spend((r.a1 - r.a0) + (r.b1 - r.b0)) {
            return Anchor::TooCommon;
        }
        self.occurrences.clear();
        for &line in &a[r.a0..r.a1] {
            self.occurrences.entry(line).or_default().count += 1;
        }
        // Positions in a second pass, and only for the lines that survived the
        // threshold: the first pass does not yet know which those are, and
        // recording every position of a line that turns out to appear four
        // hundred times is the allocation this is avoiding.
        for (i, line) in a[r.a0..r.a1].iter().enumerate() {
            let chain = self.occurrences.get_mut(line).expect("counted above");
            if chain.count <= max && (chain.at.len() as u32) < MAX_ANCHOR_OCCURRENCES {
                chain.at.push((r.a0 + i) as u32);
            }
        }

        let mut best: Option<(u32, usize, usize, usize)> = None;
        let mut common = false;
        let mut j = r.b0;
        while j < r.b1 {
            let Some(chain) = self.occurrences.get(&b[j]) else {
                j += 1;
                continue;
            };
            common = true;
            // The threshold tightens as the search goes: once a run scoring 2
            // is in hand, a line appearing forty times cannot be part of
            // anything better, so it is not worth extending through. Not only
            // an optimisation — it changes the answer, because a *longer* run is
            // otherwise allowed to win on length alone however common its lines
            // are. git does this and it is what closes the last gap: without it
            // this repository's whole history came out 10 changed lines worse
            // than `git diff --histogram` over 9,958.
            if chain.count > best.map_or(max, |(score, ..)| score.max(1)) {
                j += 1;
                continue;
            }
            // How far past `j` the longest run reaches, so the rest of a run
            // already measured is not measured again from its second line. Only
            // the *forward* extent: a run can start before `j`, and skipping by
            // its whole length would step over lines that are not in it.
            let mut skip = 0;
            for &pos in &chain.at {
                let i = pos as usize;
                let mut score = chain.count;
                let mut back = 0;
                while i - back > r.a0 && j - back > r.b0 && a[i - back - 1] == b[j - back - 1] {
                    back += 1;
                    score = score.min(self.occurrences[&a[i - back]].count);
                }
                let mut ahead = 0;
                while i + ahead + 1 < r.a1
                    && j + ahead + 1 < r.b1
                    && a[i + ahead + 1] == b[j + ahead + 1]
                {
                    ahead += 1;
                    score = score.min(self.occurrences[&a[i + ahead]].count);
                }
                let len = back + 1 + ahead;
                skip = skip.max(ahead);
                // Charged here and not only for the index build: a chain is 64
                // candidates and each may extend the length of the region, so
                // one region can cost 64 times its size and the bound has to
                // see that. Not through `spend`, which would need `&mut self`
                // while the chain is borrowed.
                self.steps = self.steps.saturating_sub(len);
                if self.steps == 0 {
                    return Anchor::TooCommon;
                }
                let better = match best {
                    None => true,
                    Some((bs, bl, _, _)) => score < bs || len > bl,
                };
                if better {
                    best = Some((score, len, i - back, j - back));
                }
            }
            j += skip + 1;
        }

        match best {
            Some((_, len, a_at, b_at)) => Anchor::At { a_at, b_at, len },
            None if common => Anchor::TooCommon,
            None => Anchor::Disjoint,
        }
    }
}

enum Anchor {
    /// Not one line of the region appears on both sides.
    Disjoint,
    /// Lines in common, none of them rare enough to anchor on.
    TooCommon,
    /// `len` matching lines starting at `a_at` and `b_at`.
    At {
        a_at: usize,
        b_at: usize,
        len: usize,
    },
}

/// A split point, unless it is a corner of the region it came from.
fn inside(r: Region, x: usize, y: usize, n: usize, m: usize) -> Option<(usize, usize)> {
    if (x == 0 && y == 0) || (x == n && y == m) {
        return None;
    }
    Some((r.a0 + x, r.b0 + y))
}

/// Furthest-reaching path per diagonal, indexed by `x - y`, which runs negative.
#[derive(Debug, Default)]
struct V {
    offset: isize,
    buf: Vec<usize>,
}

impl V {
    /// Sizes the frontier for a region's `span`, growing the buffer once and
    /// never shrinking it. A larger span moves the offset, so stale contents
    /// from a previous region land at different indices than they were written
    /// to — which is fine: only diagonals this call seeds are ever read.
    fn grow(&mut self, span: usize) {
        // Diagonals reach |k| = dmax, and the recurrence reads k+1 at the edge.
        self.offset = (span.div_ceil(2) + 3) as isize;
        let need = 2 * self.offset as usize + 1;
        if self.buf.len() < need {
            self.buf.resize(need, 0);
        }
    }

    #[inline]
    fn get(&self, k: isize) -> usize {
        self.buf[(k + self.offset) as usize]
    }

    #[inline]
    fn set(&mut self, k: isize, v: usize) {
        let i = (k + self.offset) as usize;
        self.buf[i] = v;
    }
}

// ------------------------------------------------------------ hunk assembly

/// The edit script as unified hunks: line numbers on both sides, `context`
/// unchanged lines around each change, and neighbouring changes merged when
/// their context regions touch.
///
/// Shared by every [`Differ`] on purpose. An implementation decides which lines
/// correspond; none of them should be re-deriving line numbering, and a hunk
/// header that disagrees with the lines under it is a bug that survives review.
pub fn hunks(old: &[Arc<str>], new: &[Arc<str>], edits: &[Edit], context: usize) -> Vec<Hunk> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < edits.len() {
        // Grow the group while the next change is close enough that the context
        // between them would be printed anyway. Splitting there would emit the
        // same lines twice under two headers, which is what git avoids too.
        let mut j = i;
        while j + 1 < edits.len()
            && edits[j + 1].old_start as usize <= edits[j].old_end as usize + 2 * context
        {
            j += 1;
        }
        let (first, last) = (edits[i], edits[j]);

        // The same number of lines off both sides, so the two cursors stay
        // aligned. Taking `context` from each independently drifts them apart at
        // the top of a file.
        let lead = context
            .min(first.old_start as usize)
            .min(first.new_start as usize);
        let trail = context
            .min(old.len() - last.old_end as usize)
            .min(new.len() - last.new_end as usize);
        let o_start = first.old_start as usize - lead;
        let n_start = first.new_start as usize - lead;
        let o_end = last.old_end as usize + trail;
        let n_end = last.new_end as usize + trail;

        let mut lines = Vec::with_capacity(o_end - o_start + n_end - n_start);
        let mut o = o_start;
        let mut n = n_start;
        for e in &edits[i..=j] {
            while o < e.old_start as usize {
                lines.push(line(LineKind::Context, Some(o), Some(n), &old[o]));
                o += 1;
                n += 1;
            }
            for k in e.old_range() {
                lines.push(line(LineKind::Removed, Some(k), None, &old[k]));
            }
            for k in e.new_range() {
                lines.push(line(LineKind::Added, None, Some(k), &new[k]));
            }
            o = e.old_end as usize;
            n = e.new_end as usize;
        }
        while o < o_end {
            lines.push(line(LineKind::Context, Some(o), Some(n), &old[o]));
            o += 1;
            n += 1;
        }

        out.push(Hunk {
            header: header(
                o_start,
                o_end - o_start,
                n_start,
                n_end - n_start,
                old,
                o_start,
            ),
            lines,
        });
        i = j + 1;
    }
    out
}

/// One row of a hunk.
///
/// The text is the caller's handle, shared — never copied. Acquisition hands
/// over `Arc`s so this can be a refcount bump; `Arc::from` here would put every
/// changed line in memory twice for the life of a load.
fn line(kind: LineKind, old_no: Option<usize>, new_no: Option<usize>, text: &Arc<str>) -> DiffLine {
    DiffLine {
        kind,
        // Both sides are 0-based here and 1-based on screen.
        old_no: old_no.map(|n| n as u32 + 1),
        new_no: new_no.map(|n| n as u32 + 1),
        text: Arc::clone(text),
        // Set afterwards by `mark_moved`, once the whole script is known: a
        // block that moved cannot be recognised from one hunk.
        moved: false,
    }
}

/// `@@ -41,9 +41,11 @@ fn dispatch() {`
///
/// Byte-for-byte what git writes, including the two conventions that look like
/// mistakes: a count of 1 is omitted, and a count of 0 prints the line *before*
/// the change rather than the line at it.
fn header(
    o_start: usize,
    o_count: usize,
    n_start: usize,
    n_count: usize,
    old: &[Arc<str>],
    from: usize,
) -> String {
    let range = |start: usize, count: usize| match count {
        // Nothing on this side: git names the line the change sits *after*, and
        // spells the zero out. `@@ -0,0 +1,5 @@` is a new file.
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{count}", start + 1),
    };
    let mut h = format!(
        "@@ -{} +{} @@",
        range(o_start, o_count),
        range(n_start, n_count)
    );
    if let Some(name) = enclosing(old, from) {
        h.push(' ');
        h.push_str(name);
    }
    h
}

/// How far back a hunk header looks for the declaration it sits inside.
///
/// git scans to the start of the file, and so did this, which is O(file) per
/// hunk: a 200k-line file of nothing but indented lines — a formatted JSON blob,
/// say — with five hundred hunks scans the whole thing five hundred times. A
/// declaration four hundred lines above the hunk is not useful context anyway,
/// so the cliff is not worth keeping for the sake of matching git on a string
/// nobody would have read.
const FUNCNAME_LOOKBACK: usize = 400;

/// The nearest declaration above the hunk, for the tail of its header.
///
/// git's default rule and nothing cleverer: a line starting with a letter, `_`
/// or `$` is a declaration, anything indented is inside one. It is wrong on
/// plenty of languages and right often enough to be worth the four lines — and
/// a language that cares can ship a `Differ` that writes its own headers.
fn enclosing(old: &[Arc<str>], from: usize) -> Option<&str> {
    old[from.saturating_sub(FUNCNAME_LOOKBACK)..from]
        .iter()
        .rev()
        .find_map(|l| {
            let c = *l.as_bytes().first()?;
            (c.is_ascii_alphabetic() || c == b'_' || c == b'$').then(|| l.trim_end())
        })
}

// ------------------------------------------------------------- how lines match

/// How much whitespace has to match for two lines to count as the same.
///
/// Not a different algorithm — a different *equivalence relation*, which is why
/// it is a knob on [`Differs`] rather than three more implementations. Normalising
/// is per line and length-preserving, so an edit script computed over the
/// normalised text still addresses the original lines and the hunks show the real
/// text. That is also what `git -w` does: a line whose only change was whitespace
/// comes out as context, showing the version from the old file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Whitespace {
    /// Byte for byte. git's default.
    #[default]
    Exact,
    /// Trailing whitespace only. git's `--ignore-space-at-eol`.
    Trailing,
    /// Any run of whitespace equals any other, and trailing whitespace is gone.
    /// git's `-b` / `--ignore-space-change`.
    ///
    /// Note what this does *not* do: a run collapses to one space rather than
    /// vanishing, so `foo` and ` foo` still differ. Indentation changing from two
    /// spaces to a tab does not.
    Change,
    /// All of it, anywhere. git's `-w` / `--ignore-all-space`.
    All,
}

impl Whitespace {
    pub const ALL: [Whitespace; 4] = [
        Whitespace::Exact,
        Whitespace::Trailing,
        Whitespace::Change,
        Whitespace::All,
    ];

    /// The name a config file and a picker use.
    pub fn name(self) -> &'static str {
        match self {
            Whitespace::Exact => "exact",
            Whitespace::Trailing => "trailing",
            Whitespace::Change => "change",
            Whitespace::All => "all",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.name() == name)
    }

    /// One line as this relation sees it. `None` means "unchanged", so the
    /// common case allocates nothing. Test-only: the pipeline asks [`Self::keys`]
    /// for interned ids instead of materialising one of these per line.
    #[cfg(test)]
    fn key(self, line: &str) -> Option<String> {
        match self {
            Whitespace::Exact => None,
            _ => {
                let mut out = String::with_capacity(line.len());
                self.normalize(line, &mut out);
                Some(out)
            }
        }
    }

    /// Writes the relation's form of `line` into `out`, which the caller
    /// reuses across lines. Per line and length-preserving, so an edit script
    /// computed over these still addresses the original lines.
    fn normalize(self, line: &str, out: &mut String) {
        match self {
            Whitespace::Exact => unreachable!("Exact needs no key"),
            Whitespace::Trailing => out.push_str(line.trim_end()),
            Whitespace::All => out.extend(line.chars().filter(|c| !c.is_whitespace())),
            Whitespace::Change => {
                let mut space = false;
                for c in line.trim_end().chars() {
                    match c.is_whitespace() {
                        true => space = true,
                        false => {
                            if space && !out.is_empty() {
                                out.push(' ');
                            } else if space {
                                // A leading run still collapses to one space
                                // rather than vanishing — `-b` keeps the fact
                                // that the line was indented at all.
                                out.push(' ');
                            }
                            space = false;
                            out.push(c);
                        }
                    }
                }
            }
        }
    }

    /// Handles and dense ids for every line's form under this relation,
    /// interned through `arena`. Equal text shares one allocation and one id.
    fn keys(
        self,
        lines: &[Arc<str>],
        arena: &mut KeyArena,
        out: &mut Vec<Arc<str>>,
        ids: &mut Vec<u32>,
    ) {
        match self {
            Whitespace::Exact => {}
            _ => {
                out.clear();
                out.reserve(lines.len());
                ids.clear();
                ids.reserve(lines.len());
                // Taken out so `intern` can touch the arena while the scratch
                // is borrowed; handed back afterwards.
                let mut norm = std::mem::take(&mut arena.norm);
                for line in lines {
                    norm.clear();
                    self.normalize(line, &mut norm);
                    let (id, key) = arena.intern(&norm);
                    ids.push(id);
                    out.push(key);
                }
                arena.norm = norm;
            }
        }
    }
}

/// An interning arena for normalized line keys.
///
/// Distinct content maps to one shared handle; insertion compares actual bytes
/// against every id its hash bucket names, so a hash collision can pair two
/// different lines only by being proven equal — which it cannot be. Raw hashes
/// alone would not do: two colliding keys would silently diff as equal.
#[derive(Default)]
struct KeyArena {
    /// Every distinct key, owned once. Callers get clones of the handle back,
    /// so a file costs an allocation per *distinct* line and a refcount bump
    /// per occurrence of it.
    keys: Vec<Arc<str>>,
    /// Hash of a key to the ids that share it. Almost always one entry long;
    /// collisions extend it instead of lying.
    ///
    /// On [`crate::FxHasher`] like everything else, and here that saves two
    /// hashes rather than one: the key was run through SipHash to get the `u64`,
    /// and then the `u64` was run through SipHash again to place it. Whitespace
    /// modes were 2–4× the cost of `Exact` and most of the gap was this.
    buckets: crate::FxHashMap<u64, Vec<u32>>,
    /// Scratch for [`Whitespace::normalize`], reused across lines.
    norm: String,
}

impl KeyArena {
    /// The id and handle for `key`'s content, inserting it when new. Ids are
    /// dense from zero and equal exactly when the content is, which is what lets
    /// a differ compare `u32`s without hashing the text a second time.
    fn intern(&mut self, key: &str) -> (u32, Arc<str>) {
        let mut hasher = crate::FxHasher::default();
        hasher.write(key.as_bytes());
        let hash = hasher.finish();
        if let Some(ids) = self.buckets.get(&hash) {
            for &id in ids {
                if &*self.keys[id as usize] == key {
                    return (id, Arc::clone(&self.keys[id as usize]));
                }
            }
        }
        let id = self.keys.len() as u32;
        self.keys.push(Arc::from(key));
        self.buckets.entry(hash).or_default().push(id);
        (id, Arc::clone(&self.keys[id as usize]))
    }
}

/// A frontend's live overrides of the configured behaviour.
///
/// Names and values rather than a second [`Differs`]: the registry belongs to the
/// shared `Host`, is immutable, and may hold an extension's differ — building a
/// copy of it to express "the same, but myers" would lose that. `None` on a field
/// means "whatever was configured".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub algorithm: Option<String>,
    pub whitespace: Option<Whitespace>,
}

impl Overrides {
    pub fn algorithm(name: impl Into<String>) -> Self {
        Self {
            algorithm: Some(name.into()),
            ..Default::default()
        }
    }
}

// ------------------------------------------------------------- moved blocks

/// The shortest block worth calling a move.
///
/// Two identical lines are a coincidence — `}` and a blank line are everywhere —
/// and reporting them as a move is noise on top of a diff that was legible
/// without it. Git's `--color-moved=zebra` uses 3 for the same reason.
pub const MIN_MOVED_LINES: usize = 3;

/// Which removed and added lines belong to a block that moved.
///
/// Two bitmaps rather than a list of pairs, because the only question anyone asks
/// is "is *this* line part of a move" — once per line, while assembling hunks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Moves {
    old: Vec<bool>,
    new: Vec<bool>,
}

impl Moves {
    /// No moves at all, and the cheap answer to every query.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        !self.old.iter().any(|b| *b) && !self.new.iter().any(|b| *b)
    }

    /// How many lines are part of some move, for a report.
    pub fn len(&self) -> usize {
        self.old.iter().filter(|b| **b).count() + self.new.iter().filter(|b| **b).count()
    }

    pub fn in_old(&self, line: usize) -> bool {
        self.old.get(line).copied().unwrap_or(false)
    }

    pub fn in_new(&self, line: usize) -> bool {
        self.new.get(line).copied().unwrap_or(false)
    }
}

/// Blocks that were deleted here and added there, rather than changed.
///
/// A post-pass over the edit script, not a differ: a move is only visible once
/// the whole script is known, and every algorithm produces the same one.
///
/// The rule is a run of `min` or more removed lines whose text appears, in the
/// same order, as a run of added lines. Greedy and longest-first from each start,
/// which is what git does — an exact optimum would be a matching problem, and the
/// difference is not visible on a screen.
pub fn moves(old: &[Arc<str>], new: &[Arc<str>], edits: &[Edit], min: usize) -> Moves {
    if min == 0 {
        return Moves::none();
    }
    // Only the lines the script actually touched: a moved block is by definition
    // removed on one side and added on the other, and matching against unchanged
    // lines would call every repeated line a move.
    let mut removed = vec![false; old.len()];
    let mut added = vec![false; new.len()];
    for e in edits {
        removed[e.old_range()].fill(true);
        added[e.new_range()].fill(true);
    }

    // Where each added line's text can be found. Built over the added lines
    // only, so a line that also exists unchanged elsewhere is not a candidate.
    let mut index: crate::FxHashMap<&Arc<str>, Vec<u32>> = crate::FxHashMap::default();
    for (i, line) in new.iter().enumerate() {
        if added[i] && !line.trim().is_empty() {
            index.entry(line).or_default().push(i as u32);
        }
    }

    let mut out = Moves {
        old: vec![false; old.len()],
        new: vec![false; new.len()],
    };
    let mut taken = vec![false; new.len()];
    let mut i = 0;
    while i < old.len() {
        if !removed[i] {
            i += 1;
            continue;
        }
        // The longest run starting here, over every place its first line landed.
        let mut best = (0usize, 0usize);
        for &start in index.get(&old[i]).map(Vec::as_slice).unwrap_or(&[]) {
            let start = start as usize;
            let mut len = 0;
            while i + len < old.len()
                && start + len < new.len()
                && removed[i + len]
                && added[start + len]
                && !taken[start + len]
                && old[i + len] == new[start + len]
            {
                len += 1;
            }
            if len > best.0 {
                best = (len, start);
            }
        }
        if best.0 < min {
            i += 1;
            continue;
        }
        let (len, start) = best;
        out.old[i..i + len].fill(true);
        out.new[start..start + len].fill(true);
        // Claimed, so a block deleted once and added twice marks one landing
        // site rather than reporting the same lines as two moves.
        taken[start..start + len].fill(true);
        i += len;
    }
    out
}

/// Marks the lines of `hunks` that [`moves`] found.
///
/// After assembly rather than during it, so `hunks` stays a pure function of the
/// edit script and nothing about move detection reaches it.
pub fn mark_moved(hunks: &mut [Hunk], m: &Moves) {
    for line in hunks.iter_mut().flat_map(|h| &mut h.lines) {
        line.moved = match line.kind {
            LineKind::Removed => line.old_no.is_some_and(|n| m.in_old(n as usize - 1)),
            LineKind::Added => line.new_no.is_some_and(|n| m.in_new(n as usize - 1)),
            LineKind::Context => false,
        };
    }
}

// ------------------------------------------------------- the indent heuristic

/// Slides each edit to the most readable of its equivalent positions.
///
/// A run of changed lines can often sit in several places that describe exactly
/// the same change: if the line leaving the top of the group equals the line
/// entering the bottom, the whole group can shift by one and mean the same thing.
/// Which of those positions a reader wants is not arbitrary — a hunk that starts
/// at a function's signature reads; the same hunk starting at the previous
/// function's closing brace does not.
///
/// This is git's `--indent-heuristic`, on by default there since 2.14 and ported
/// rather than reinvented for one reason: it is the only version whose output can
/// be *checked*. `git/examples/diffcheck.rs` compares hunk counts, and an
/// approximation of these weights would differ from git in a way no test could
/// call right or wrong. The constants below are xdiff's, names and values.
pub fn compact(old: &[Arc<str>], new: &[Arc<str>], edits: &mut [Edit]) {
    compact_with(old, new, old, new, edits)
}

/// [`compact`] where the text to *score* and the text to *compare* differ.
///
/// They differ whenever a [`Whitespace`] relation is in play, and conflating them
/// is a bug in each direction. A slide is possible when the relation says the
/// line leaving one end equals the line entering the other — so equality has to
/// use the keys, or `-w` cannot slide across a reindented line and git can. How
/// *readable* the result is depends on the real indentation — so scoring has to
/// use the text, because the keys are the thing that erased it.
/// `K` is one side's key type — the lines' own handles when the relation is
/// byte-for-byte, interned normalized handles when it is not. Equality is all a
/// slide asks of it.
pub fn compact_with<K: PartialEq>(
    old: &[Arc<str>],
    new: &[Arc<str>],
    old_keys: &[K],
    new_keys: &[K],
    edits: &mut [Edit],
) {
    for i in 0..edits.len() {
        // The window this group may slide within: not past its neighbours, and
        // not off either end of the file.
        let lo = if i == 0 {
            0
        } else {
            edits[i - 1].old_end as usize
        };
        let hi = match edits.get(i + 1) {
            Some(next) => next.old_start as usize,
            None => old.len(),
        };
        slide(old, new, old_keys, new_keys, &mut edits[i], lo, hi);
    }
}

/// Shifts one edit within `lo..hi` to the best-scoring equivalent position.
#[allow(clippy::too_many_arguments)]
fn slide<K: PartialEq>(
    old: &[Arc<str>],
    new: &[Arc<str>],
    old_keys: &[K],
    new_keys: &[K],
    e: &mut Edit,
    lo: usize,
    hi: usize,
) {
    // Only one side may be empty for a slide to be meaningful. A replace has both
    // sides moving, and git slides those in each file independently through
    // machinery this does not have; its boundaries are pinned on both sides
    // anyway, so the case is rare.
    let (lines, keys, start, end) = match (e.old_range().len(), e.new_range().len()) {
        (0, 0) => return,
        (0, _) => (new, new_keys, e.new_start as usize, e.new_end as usize),
        (_, 0) => (old, old_keys, e.old_start as usize, e.old_end as usize),
        _ => return,
    };
    let len = end - start;
    if len == 0 {
        return;
    }

    // Every position the group can occupy, found by walking it as far as it will
    // go each way: the group can shift by one whenever the line leaving one end
    // equals the line entering the other.
    let mut lowest = start;
    while lowest > 0 && keys[lowest - 1] == keys[lowest + len - 1] {
        lowest -= 1;
    }
    let mut highest = start;
    while highest + len < keys.len() && keys[highest] == keys[highest + len] {
        highest += 1;
    }
    // Never over a neighbouring change: two edits that overlap describe nothing,
    // and `verify` is the only thing that would notice.
    if e.new_range().is_empty() {
        lowest = lowest.max(lo);
        highest = highest.min(hi.saturating_sub(len));
    }
    if lowest >= highest {
        return;
    }
    // Bounded like git's, so a group inside ten thousand identical lines does not
    // score ten thousand positions.
    if highest - lowest > INDENT_HEURISTIC_MAX_SLIDING {
        return;
    }

    let mut best = (score_at(lines, lowest, len), lowest);
    for at in lowest + 1..=highest {
        let score = score_at(lines, at, len);
        // `<=`, so a tie goes to the *later* position. git's, and it matters:
        // ties are common and the two answers are visibly different.
        if score_cmp(&score, &best.0) <= 0 {
            best = (score, at);
        }
    }
    let shift = best.1 as i64 - start as i64;
    e.old_start = (e.old_start as i64 + shift) as u32;
    e.old_end = (e.old_end as i64 + shift) as u32;
    e.new_start = (e.new_start as i64 + shift) as u32;
    e.new_end = (e.new_end as i64 + shift) as u32;
}

// xdiff's weights, names and values. Do not tune these without re-running
// `diffcheck` — they are the reason our hunk boundaries match git's.
const MAX_INDENT: i64 = 200;
const MAX_BLANKS: i64 = 20;
const INDENT_WEIGHT: i64 = 60;
const INDENT_HEURISTIC_MAX_SLIDING: usize = 100;
const START_OF_FILE_PENALTY: i64 = 1;
const END_OF_FILE_PENALTY: i64 = 21;
const TOTAL_BLANK_WEIGHT: i64 = -30;
const POST_BLANK_WEIGHT: i64 = 6;
const RELATIVE_INDENT_PENALTY: i64 = -4;
const RELATIVE_INDENT_WITH_BLANK_PENALTY: i64 = 10;
const RELATIVE_OUTDENT_PENALTY: i64 = 24;
const RELATIVE_OUTDENT_WITH_BLANK_PENALTY: i64 = 17;
const RELATIVE_DEDENT_PENALTY: i64 = 23;
const RELATIVE_DEDENT_WITH_BLANK_PENALTY: i64 = 17;

/// Indentation in columns, tabs to the next multiple of eight. `None` for a line
/// that is only whitespace, which has no indentation to compare.
///
/// Eight rather than the four `markdown::column_indent` uses, and other
/// whitespace characters advance nothing without ending the run: both are
/// xdiff's, because this is the function whose answers are being checked against
/// git's.
fn indent_of(line: &str) -> Option<i64> {
    let mut n = 0i64;
    for b in line.bytes() {
        match b {
            b' ' => n += 1,
            b'\t' => n += 8 - (n % 8),
            b'\r' | b'\n' | 0x0b | 0x0c => {}
            _ => return Some(n),
        }
        if n >= MAX_INDENT {
            return Some(MAX_INDENT);
        }
    }
    None
}

/// What one split point looks like to the heuristic.
struct Measure {
    end_of_file: bool,
    /// The line at the split. `None` when it is blank or past the end.
    indent: Option<i64>,
    pre_blank: i64,
    pre_indent: Option<i64>,
    post_blank: i64,
    post_indent: Option<i64>,
}

fn measure(lines: &[Arc<str>], split: usize) -> Measure {
    let (end_of_file, indent) = match split >= lines.len() {
        true => (true, None),
        false => (false, indent_of(&lines[split])),
    };

    let mut pre_blank = 0;
    let mut pre_indent = None;
    let mut i = split;
    while i > 0 {
        i -= 1;
        pre_indent = indent_of(&lines[i]);
        if pre_indent.is_some() {
            break;
        }
        pre_blank += 1;
        if pre_blank == MAX_BLANKS {
            // Far enough: treat it as flush left rather than as unknown, so a
            // group after twenty blank lines is not scored as start-of-file.
            pre_indent = Some(0);
            break;
        }
    }

    let mut post_blank = 0;
    let mut post_indent = None;
    let mut j = split + 1;
    while j < lines.len() {
        post_indent = indent_of(&lines[j]);
        if post_indent.is_some() {
            break;
        }
        post_blank += 1;
        if post_blank == MAX_BLANKS {
            post_indent = Some(0);
            break;
        }
        j += 1;
    }

    Measure {
        end_of_file,
        indent,
        pre_blank,
        pre_indent,
        post_blank,
        post_indent,
    }
}

/// A position's badness, in the two parts git keeps separate.
///
/// `effective_indent` is compared by *sign* and not by magnitude — that is the
/// part that is easy to get wrong, and getting it wrong produces slides that look
/// plausible and disagree with git. Ported as two fields plus [`score_cmp`] for
/// exactly that reason.
#[derive(Default, Clone, Copy)]
struct Score {
    effective_indent: i64,
    penalty: i64,
}

fn score_at(lines: &[Arc<str>], at: usize, len: usize) -> Score {
    let mut s = Score::default();
    add_split(&measure(lines, at), &mut s);
    add_split(&measure(lines, at + len), &mut s);
    s
}

fn add_split(m: &Measure, s: &mut Score) {
    if m.pre_indent.is_none() && m.pre_blank == 0 {
        s.penalty += START_OF_FILE_PENALTY;
    }
    if m.end_of_file {
        s.penalty += END_OF_FILE_PENALTY;
    }

    // Blank lines *following* the split, counting the line at it when that line
    // is itself blank.
    let post_blank = match m.indent {
        None => 1 + m.post_blank,
        Some(_) => 0,
    };
    let total_blank = m.pre_blank + post_blank;
    s.penalty += TOTAL_BLANK_WEIGHT * total_blank;
    s.penalty += POST_BLANK_WEIGHT * post_blank;

    // A blank line takes the indentation of whatever follows it, so a break
    // before a run of blanks is judged by the code after them.
    let indent = m.indent.or(m.post_indent);
    let any_blanks = total_blank != 0;
    // -1 at the end of the file, which is what makes a break there compare
    // favourably on indent and unfavourably on penalty.
    s.effective_indent += indent.unwrap_or(-1);

    let (Some(indent), Some(pre)) = (indent, m.pre_indent) else {
        return;
    };
    if indent > pre {
        // More indented than what came before: likely inside a block.
        s.penalty += if any_blanks {
            RELATIVE_INDENT_WITH_BLANK_PENALTY
        } else {
            RELATIVE_INDENT_PENALTY
        };
    } else if indent == pre {
        // Same level. Nothing to say.
    } else if m.post_indent.is_some_and(|post| post > indent) {
        // Less indented, and what follows is more: this line opens a block —
        // an `else`, or a signature. A good place to break, relatively.
        s.penalty += if any_blanks {
            RELATIVE_OUTDENT_WITH_BLANK_PENALTY
        } else {
            RELATIVE_OUTDENT_PENALTY
        };
    } else {
        // Less indented and nothing opens after it: the end of a block.
        s.penalty += if any_blanks {
            RELATIVE_DEDENT_WITH_BLANK_PENALTY
        } else {
            RELATIVE_DEDENT_PENALTY
        };
    }
}

/// Negative when `a` is the better place to break.
///
/// The indent comparison is three-way and then weighted, rather than the
/// difference being weighted: a position one column further left wins by exactly
/// as much as one a hundred columns further left. That is git's, and it is the
/// whole reason this is a function and not a subtraction.
fn score_cmp(a: &Score, b: &Score) -> i64 {
    let indents = match a.effective_indent.cmp(&b.effective_indent) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    };
    INDENT_WEIGHT * indents + (a.penalty - b.penalty)
}

// ----------------------------------------------------------------- the cache

/// How many answers the cache may hold before its oldest entry is forgotten.
///
/// An entry count and not a byte count, on purpose: bytes would need a pass
/// over every hunk to know. 4096 files is far past any one diff — cmux at
/// `HEAD~120..HEAD` is 482 — and far below anything that could crowd memory,
/// while a walk of a 500k-commit history commit by commit simply forgets its
/// beginning instead of growing without bound.
const CACHE_CAP: usize = 4096;

/// What one cached answer is keyed on: **identity plus every setting that
/// reaches the answer.**
///
/// The OIDs are identity — a blob's content never changes, so the pair names
/// both sides' text completely, renames included (the hunks of a rename are
/// computed from content alone; only the *label* carries paths). The rest is
/// the resolved answer of [`Differs::file_using`] itself: which algorithm ran
/// *after* routing and overrides, which whitespace relation, and the three
/// configuration knobs that shape hunks from an edit script.
///
/// The key is built in the same function that resolves those values — never
/// re-derived by a caller — because a second derivation is where drift lives:
/// a knob added to [`Differs`] tomorrow has to appear here once or every
/// cached answer after a change of it is a lie. That is also why the path is
/// *not* in the key: two paths resolving to the same algorithm and relation
/// with the same blobs genuinely have the same hunks, and `.txt` renamed to
/// `.rs` changes the routed algorithm, which changes the key on its own.
///
/// Forward note: the key is field-by-field today, so its maintenance rule is
/// memory — a knob added to [`Overrides`] or to this registry that can change
/// hunk output must join it, or stale answers survive a turn of that knob in
/// silence. If that list grows, the structural fix is hoisting the settings
/// into one `Hash` struct embedded wholesale, so a new field lands in the key
/// by construction instead of by recall.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    old: String,
    new: String,
    algorithm: &'static str,
    whitespace: Whitespace,
    context: u32,
    min_moved: u32,
    indent_heuristic: bool,
}

/// Bounded, insertion-order-evicting store of assembled answers.
///
/// A plain map plus the order keys arrived in. No LRU touching, no per-entry
/// locks, no reference counting scheme: a hit clones the answer, a miss
/// computes outside the lock and inserts, and when the cap is crossed the
/// front of the queue goes. Keys enter the queue exactly once each — only on
/// the vacant side of an entry — so the queue and the map cannot disagree.
#[derive(Default)]
struct Cache {
    answers: crate::FxHashMap<Key, Vec<Hunk>>,
    order: VecDeque<Key>,
}

impl Cache {
    fn get(&self, key: &Key) -> Option<Vec<Hunk>> {
        self.answers.get(key).cloned()
    }

    /// Records an answer, unless another thread recorded it first — two cold
    /// misses racing compute the same file twice and keep the first result;
    /// determinism makes them identical, so dropping either is safe and this
    /// keeps one canonical copy alive for every later clone to come from.
    fn put(&mut self, key: Key, hunks: Vec<Hunk>) {
        // One lock guards lookup through insertion, so this check and the
        // insert below cannot interleave with another thread's.
        if self.answers.contains_key(&key) {
            return;
        }
        self.order.push_back(key.clone());
        if self.order.len() > CACHE_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.answers.remove(&oldest);
            }
        }
        self.answers.insert(key, hunks);
    }
}

// ---------------------------------------------------------------- the registry

/// Which algorithm each path gets, and how much context its hunks carry.
///
/// The same shape as [`Highlighters`](crate::syntax::Highlighters), for the same
/// reason: routing by path is what lets a specialist take `.json` without the
/// generalist knowing it exists. Selection is by *name* rather than by value
/// because a config file has to be able to express it — see
/// `docs/decisions/0012-config-is-data-behaviour-is-not.md`.
#[derive(Clone)]
pub struct Differs {
    impls: Vec<Arc<dyn Differ>>,
    routes: Vec<(Vec<String>, usize)>,
    fallback: usize,
    /// Unchanged lines shown around each change. git's default is 3.
    pub context: usize,
    /// How much whitespace has to match for two lines to count as the same.
    pub whitespace: Whitespace,
    /// Shortest block reported as a move; `0` turns detection off.
    pub min_moved: usize,
    /// Slide each change to the most readable of its equivalent positions. On,
    /// as it is in git.
    pub indent_heuristic: bool,
    /// Assembled answers, keyed on blob pair and settings — see [`Cache`].
    ///
    /// Behind an `Arc` so that a clone of the registry — which panes make to
    /// diff on their own thread — shares this one cache rather than forking
    /// it, and behind a lock because two panes may refresh at once. The cache
    /// dies with the registry, i.e. with the host: a config reload rebuilds
    /// both, and a reload is exactly when context or algorithm may have moved
    /// anyway. The key covers those settings regardless; the lifetime is
    /// hygiene, not correctness.
    cache: Arc<Mutex<Cache>>,
}

impl Default for Differs {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Differs {
    /// The three shipped algorithms, with Histogram selected.
    pub fn builtin() -> Self {
        let mut d = Self {
            impls: Vec::new(),
            routes: Vec::new(),
            fallback: 0,
            context: 3,
            whitespace: Whitespace::Exact,
            min_moved: MIN_MOVED_LINES,
            indent_heuristic: true,
            cache: Arc::default(),
        };
        d.register(Histogram::default());
        d.register(Patience::default());
        d.register(Myers::default());
        d.select("histogram");
        d
    }

    /// Adds an implementation, replacing any already registered under the same
    /// name — so a built-in can be corrected rather than only added to, exactly
    /// as a language table can.
    pub fn register(&mut self, differ: impl Differ + 'static) {
        match self.impls.iter().position(|d| d.name() == differ.name()) {
            Some(i) => {
                self.impls[i] = Arc::new(differ);
                // A replacement keeps its name but not its answers, and the
                // key cannot tell them apart — drop everything rather than
                // serve hunks under a name that did not produce them.
                *self.locked() = Cache::default();
            }
            None => self.impls.push(Arc::new(differ)),
        }
    }

    /// Which algorithm everything unrouted uses. False when nothing is
    /// registered under that name, which is what a config file reports back.
    pub fn select(&mut self, name: &str) -> bool {
        match self.position(name) {
            Some(i) => {
                self.fallback = i;
                true
            }
            None => false,
        }
    }

    /// Keys are extensions or whole filenames, matched the way
    /// [`Highlighters::route`](crate::syntax::Highlighters::route) matches them.
    /// A later route wins.
    pub fn route(&mut self, keys: &[&str], name: &str) -> bool {
        match self.position(name) {
            Some(i) => {
                self.routes
                    .push((keys.iter().map(|k| k.to_ascii_lowercase()).collect(), i));
                true
            }
            None => false,
        }
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.impls.iter().position(|d| d.name() == name)
    }

    /// Every registered name, for a config error message that says what the
    /// options actually are rather than what they were when it was written.
    pub fn names(&self) -> Vec<&'static str> {
        self.impls.iter().map(|d| d.name()).collect()
    }

    pub fn selected(&self) -> &'static str {
        self.impls[self.fallback].name()
    }

    /// One implementation by name, for a frontend that lets you pick.
    ///
    /// A name and not a value, because the registry is the host's and may hold
    /// an extension's differ — building a second one to express "the same, but
    /// myers" would silently lose it, and rule 1 says an extension's algorithm
    /// has to be pickable exactly as a built-in's is.
    pub fn by_name(&self, name: &str) -> Option<&dyn Differ> {
        self.position(name).map(|i| self.impls[i].as_ref())
    }

    pub fn for_path(&self, path: &str) -> &dyn Differ {
        let name = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(path)
            .to_ascii_lowercase();
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_string());
        for (keys, i) in self.routes.iter().rev() {
            if keys.iter().any(|k| *k == name || Some(k) == ext.as_ref()) {
                return self.impls[*i].as_ref();
            }
        }
        self.impls[self.fallback].as_ref()
    }

    /// One file, start to finish: route it, diff it, assemble its hunks.
    ///
    /// This is what an acquisition layer calls. It never learns which algorithm
    /// ran, which is the point.
    pub fn file(&self, path: &str, old: &[Arc<str>], new: &[Arc<str>]) -> FileDiff {
        self.file_using(&Overrides::default(), path, old, new, None)
    }

    /// The same, with a frontend's live overrides applied.
    ///
    /// `Overrides::algorithm` overrides the *routes* too, deliberately: a user
    /// who asks for myers asked for the whole diff in myers, and quietly leaving
    /// `.json` on whatever it was routed to would make the control lie about what
    /// is on screen. A name that is not registered falls back to the configured
    /// behaviour rather than failing, because the caller is a click.
    ///
    /// `blobs` is the two sides' blob OIDs when both sides are real blobs in
    /// the object database — what [`gitten_git::Pair`] carries — and `None`
    /// when either side has none: a working-tree or null side, a gitlink, or
    /// any caller without identity to offer. `None` is *always compute*, and
    /// that is the whole safety story of the cache: partial identity invents
    /// keys for answers nobody proved.
    ///
    /// # The cache, and why the key is built here
    ///
    /// A repeated acquisition re-diffs every unchanged file from scratch,
    /// which is most of the cost of a shell refresh after an unrelated write.
    /// When `blobs` is present the assembled hunks are remembered under
    /// `(old_oid, new_oid)` **plus everything this function would otherwise
    /// compute fresh**: the algorithm actually selected (override, else route,
    /// else fallback), the whitespace relation in force, context, move floor
    /// and the indent heuristic. A change to any of them misses, so the cache
    /// can change what an answer costs but never what it says. See [`Cache`]
    /// for the store; see [`Key`] for why the path is not in it.
    ///
    /// On a hit the hunks are cloned, not shared: one deep-ish copy per file —
    /// refcount bumps on line text plus a small struct per line — against the
    /// interning, search, slide and move detection being skipped, microseconds
    /// against milliseconds. Sharing an `Arc<Vec<Hunk>>` would ripple that
    /// wrapper through every consumer in three clients to save half of a win
    /// the clone already banks.
    pub fn file_using(
        &self,
        over: &Overrides,
        path: &str,
        old: &[Arc<str>],
        new: &[Arc<str>],
        blobs: Option<(&str, &str)>,
    ) -> FileDiff {
        // The two resolutions below are the answer's actual inputs beyond the
        // text, which makes them the non-identity part of the cache key. They
        // happen here, once, and the same values drive both the lookup and the
        // computation — a caller never re-derives them, because a second
        // derivation could drift from this one.
        let differ = over
            .algorithm
            .as_deref()
            .and_then(|name| self.by_name(name))
            .unwrap_or_else(|| self.for_path(path));
        let ws = over.whitespace.unwrap_or(self.whitespace);

        // No identity, no entry: compute, exactly as before the cache existed.
        let Some((old_oid, new_oid)) = blobs else {
            return self.compute(path, differ, old, new, ws);
        };

        let key = Key {
            old: old_oid.to_owned(),
            new: new_oid.to_owned(),
            algorithm: differ.name(),
            whitespace: ws,
            context: self.context as u32,
            min_moved: self.min_moved as u32,
            indent_heuristic: self.indent_heuristic,
        };
        if let Some(hunks) = self.locked().get(&key) {
            return FileDiff {
                path: path.to_owned(),
                hunks,
            };
        }

        let fresh = self.compute(path, differ, old, new, ws);
        self.locked().put(key, fresh.hunks.clone());
        fresh
    }

    fn locked(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Everything between routing and a `FileDiff`: the equivalence relation's
    /// keys, then the four stages of [`Self::assemble`]. The only caller of
    /// both cache paths, so a hit and a miss are provably the same work.
    fn compute(
        &self,
        path: &str,
        differ: &dyn Differ,
        old: &[Arc<str>],
        new: &[Arc<str>],
        ws: Whitespace,
    ) -> FileDiff {
        match ws {
            // Byte-for-byte: the lines themselves are the keys, and their
            // handles are already shared.
            Whitespace::Exact => self.assemble(path, differ, old, new, old, new, None),
            _ => {
                // Normalised once per file into interned handles and ids; what
                // every stage below compares are those, equal exactly when the
                // relation says so.
                let mut arena = KeyArena::default();
                let (mut ko, mut kn) =
                    (Vec::with_capacity(old.len()), Vec::with_capacity(new.len()));
                let (mut ido, mut idn) =
                    (Vec::with_capacity(old.len()), Vec::with_capacity(new.len()));
                ws.keys(old, &mut arena, &mut ko, &mut ido);
                ws.keys(new, &mut arena, &mut kn, &mut idn);
                self.assemble(path, differ, old, new, &ko, &kn, Some((&ido, &idn)))
            }
        }
    }

    /// The four stages after routing, in the order they have to happen:
    ///
    /// 1. **Diff** — the seam. `keys_old`/`keys_new` are what it compares: the
    ///    original lines, or their form under the whitespace relation. It never
    ///    learns which.
    /// 2. **Compact**, sliding each change to the most readable of its equivalent
    ///    positions. Before hunk assembly, because it moves the changes and the
    ///    hunks are drawn around wherever they end up.
    /// 3. **Detect moves**, which needs the whole script and so cannot be step 1.
    /// 4. **Assemble hunks** against the real text, which everything downstream
    ///    addresses.
    ///
    /// Under the exact relation the keys *are* the lines — one slice passed for
    /// both; otherwise interned handles into a per-file arena. Every stage below
    /// sees exactly the equivalence relation it always did.
    #[allow(clippy::too_many_arguments)]
    fn assemble(
        &self,
        path: &str,
        differ: &dyn Differ,
        old: &[Arc<str>],
        new: &[Arc<str>],
        keys_old: &[Arc<str>],
        keys_new: &[Arc<str>],
        ids: Option<(&[u32], &[u32])>,
    ) -> FileDiff {
        // Ids when the caller interned them — the whitespace relations do,
        // because normalising a line is already a pass over it. `Exact` has
        // nothing interned yet and the differ does its own.
        let mut edits = match ids {
            Some(ids) => differ.diff_interned(path, keys_old, keys_new, ids),
            None => differ.diff(path, keys_old, keys_new),
        };
        if self.indent_heuristic {
            // Both: readability is scored against the text a reader will see, and
            // whether a slide is possible at all is decided by the relation. Two
            // hunks in cmux's history land in a different place from git's if the
            // second half of that is skipped.
            compact_with(old, new, keys_old, keys_new, &mut edits);
        }
        let mut hunks = hunks(old, new, &edits, self.context);
        let m = moves(keys_old, keys_new, &edits, self.min_moved);
        if !m.is_empty() {
            mark_moved(&mut hunks, &m);
        }
        FileDiff {
            path: path.to_string(),
            hunks,
        }
    }
}

// ----------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<Arc<str>> {
        if s.is_empty() {
            return Vec::new();
        }
        s.trim_end_matches('\n')
            .split('\n')
            .map(Arc::from)
            .collect()
    }

    /// Every property the rest of the pipeline relies on, for any differ.
    fn verify(old: &[Arc<str>], new: &[Arc<str>], edits: &[Edit]) {
        let mut o = 0usize;
        let mut n = 0usize;
        let mut rebuilt: Vec<Arc<str>> = Vec::new();
        for (i, e) in edits.iter().enumerate() {
            assert!(!e.is_empty(), "edit {i} is empty: {e:?}");
            assert!(e.old_start as usize >= o, "edit {i} out of order: {e:?}");
            assert!(e.new_start as usize >= n, "edit {i} out of order: {e:?}");
            assert!(
                e.old_start <= e.old_end && e.new_start <= e.new_end,
                "{e:?}"
            );
            assert!(
                e.old_end as usize <= old.len() && e.new_end as usize <= new.len(),
                "{e:?}"
            );
            if i > 0 {
                let p = edits[i - 1];
                assert!(
                    p.old_end < e.old_start || p.new_end < e.new_start,
                    "adjacent edits {p:?} {e:?} should have merged"
                );
            }
            // The gap since the last edit must be identical on both sides —
            // that is what makes it a context run.
            assert_eq!(
                e.old_start as usize - o,
                e.new_start as usize - n,
                "edit {i} shifts without an edit: {e:?}"
            );
            rebuilt.extend_from_slice(&old[o..e.old_start as usize]);
            rebuilt.extend_from_slice(&new[e.new_range()]);
            o = e.old_end as usize;
            n = e.new_end as usize;
        }
        rebuilt.extend_from_slice(&old[o..]);
        assert_eq!(
            rebuilt, new,
            "applying the script did not produce the new file"
        );
    }

    /// Length of the longest common subsequence, by the textbook table. The
    /// reference a minimal script is checked against.
    fn lcs(a: &[Arc<str>], b: &[Arc<str>]) -> usize {
        let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                t[i][j] = if a[i] == b[j] {
                    t[i + 1][j + 1] + 1
                } else {
                    t[i + 1][j].max(t[i][j + 1])
                };
            }
        }
        t[0][0]
    }

    fn changed(edits: &[Edit]) -> usize {
        edits
            .iter()
            .map(|e| e.old_range().len() + e.new_range().len())
            .sum()
    }

    /// The three shipped algorithms, freshly constructed — each carries its own
    /// scratch now, so a test-local value is what gets shared across its loop.
    fn all() -> [Box<dyn Differ>; 3] {
        [
            Box::new(Histogram::default()),
            Box::new(Patience::default()),
            Box::new(Myers::default()),
        ]
    }

    #[test]
    fn every_differ_produces_a_script_that_applies() {
        let old = lines("fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n");
        let new = lines("fn main() {\n    let x = 2;\n    let y = 3;\n    println!(\"{x}\");\n}\n");
        for d in all() {
            let edits = d.diff("a.rs", &old, &new);
            verify(&old, &new, &edits);
            assert!(!edits.is_empty(), "{} found no change", d.name());
        }
    }

    #[test]
    fn built_in_interned_and_text_paths_agree() {
        let old = lines("start\nrepeat\nold\nrepeat\nend\n");
        let new = lines("start\nrepeat\nnew\ninsert\nrepeat\nend\n");
        let ids = intern(&old, &new);
        for differ in all() {
            assert_eq!(
                differ.diff("x", &old, &new),
                differ.diff_interned("x", &old, &new, (&ids.0, &ids.1)),
                "{}",
                differ.name()
            );
        }
    }

    #[test]
    fn a_default_differ_ignores_ids() {
        struct Text;
        impl Differ for Text {
            fn name(&self) -> &'static str {
                "text"
            }

            fn diff(&self, _path: &str, _old: &[Arc<str>], _new: &[Arc<str>]) -> Vec<Edit> {
                vec![Edit {
                    old_start: 0,
                    old_end: 1,
                    new_start: 0,
                    new_end: 1,
                }]
            }
        }

        let old = lines("old");
        let new = lines("new");
        assert_eq!(
            Text.diff("x", &old, &new),
            Text.diff_interned("x", &old, &new, (&[0], &[0]))
        );
    }

    #[test]
    fn whitespace_keys_and_ids_agree() {
        let source = lines("a  b\na b\na\tb\ntrailing \ntrailing\n");
        for ws in [Whitespace::Trailing, Whitespace::Change, Whitespace::All] {
            let mut arena = KeyArena::default();
            let (mut keys, mut ids) = (Vec::new(), Vec::new());
            ws.keys(&source, &mut arena, &mut keys, &mut ids);
            assert_eq!(ids.len(), source.len());
            for i in 0..source.len() {
                for j in 0..source.len() {
                    assert_eq!(ids[i] == ids[j], keys[i] == keys[j], "{ws:?}: {i}, {j}");
                }
            }
        }
    }

    #[test]
    fn whitespace_ids_are_shared_across_both_sides() {
        let old = lines("left\nshared  line\n");
        let new = lines("shared line\nright\n");
        let mut arena = KeyArena::default();
        let (mut old_keys, mut new_keys) = (Vec::new(), Vec::new());
        let (mut old_ids, mut new_ids) = (Vec::new(), Vec::new());
        Whitespace::Change.keys(&old, &mut arena, &mut old_keys, &mut old_ids);
        Whitespace::Change.keys(&new, &mut arena, &mut new_keys, &mut new_ids);
        assert_eq!(old_keys[1], new_keys[0]);
        assert_eq!(old_ids[1], new_ids[0]);
    }

    #[test]
    fn identical_files_produce_no_edits() {
        let f = lines("a\nb\nc\n");
        for d in all() {
            assert!(d.diff("x", &f, &f).is_empty(), "{}", d.name());
        }
    }

    #[test]
    fn one_side_empty_is_the_whole_file() {
        let f = lines("a\nb\nc\n");
        for d in all() {
            let add = d.diff("x", &[], &f);
            verify(&[], &f, &add);
            assert_eq!(
                add,
                vec![Edit {
                    old_start: 0,
                    old_end: 0,
                    new_start: 0,
                    new_end: 3
                }]
            );
            let del = d.diff("x", &f, &[]);
            verify(&f, &[], &del);
            assert_eq!(
                del,
                vec![Edit {
                    old_start: 0,
                    old_end: 3,
                    new_start: 0,
                    new_end: 0
                }]
            );
            // And two empty files are not a change.
            assert!(d.diff("x", &[], &[]).is_empty());
        }
    }

    #[test]
    fn myers_is_minimal() {
        // The definition of the algorithm, checked against the textbook table.
        let old = lines("a\nb\nc\na\nb\nb\na\n");
        let new = lines("c\nb\na\nb\na\nc\n");
        let edits = Myers::default().diff("x", &old, &new);
        verify(&old, &new, &edits);
        let ideal = old.len() + new.len() - 2 * lcs(&old, &new);
        assert_eq!(changed(&edits), ideal, "{edits:?}");
    }

    #[test]
    fn myers_is_minimal_on_random_input() {
        // A deterministic pseudo-random sweep: the middle-snake recursion is the
        // one piece of arithmetic here that no amount of reading proves right.
        let mut seed = 0x2545f491_4f6cdd1du64;
        let mut rand = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let alphabet = ["a", "b", "c", "d", "e", "f", "g", "h"];
        for case in 0..400 {
            let (n, m) = (rand(24) as usize, rand(24) as usize);
            let letters = 2 + rand(6) as usize;
            let old: Vec<Arc<str>> = (0..n)
                .map(|_| Arc::from(alphabet[rand(letters as u64) as usize]))
                .collect();
            let new: Vec<Arc<str>> = (0..m)
                .map(|_| Arc::from(alphabet[rand(letters as u64) as usize]))
                .collect();
            let edits = Myers::default().diff("x", &old, &new);
            verify(&old, &new, &edits);
            let ideal = old.len() + new.len() - 2 * lcs(&old, &new);
            assert_eq!(
                changed(&edits),
                ideal,
                "case {case}: {old:?} -> {new:?} gave {edits:?}"
            );
        }
    }

    #[test]
    fn every_differ_applies_on_random_input() {
        // The anchored ones are not minimal by design, so only the round trip is
        // asserted — but it is asserted over the same awkward shapes.
        let mut seed = 0x853c49e6_748fea9bu64;
        let mut rand = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let alphabet = ["a", "b", "c", "d", "e", "}", "", "  "];
        for case in 0..400 {
            let (n, m) = (rand(30) as usize, rand(30) as usize);
            let letters = 1 + rand(7) as usize;
            let old: Vec<Arc<str>> = (0..n)
                .map(|_| Arc::from(alphabet[rand(letters as u64) as usize]))
                .collect();
            let new: Vec<Arc<str>> = (0..m)
                .map(|_| Arc::from(alphabet[rand(letters as u64) as usize]))
                .collect();
            for d in all() {
                let edits = d.diff("x", &old, &new);
                verify(&old, &new, &edits);
                assert!(
                    changed(&edits) <= old.len() + new.len(),
                    "case {case} {}: {edits:?}",
                    d.name()
                );
            }
        }
    }

    #[test]
    fn histogram_reads_a_moved_block_as_a_move() {
        // The reason histogram is the default. Both signatures appear once, so
        // both are anchors, and the identical braces never get to pair
        // themselves up: the answer is one block deleted and one inserted.
        let old = lines("fn one() {\n    a();\n}\nfn two() {\n    b();\n}\n");
        let new = lines("fn two() {\n    b();\n}\nfn one() {\n    a();\n}\n");
        let h = Histogram::default().diff("x.rs", &old, &new);
        verify(&old, &new, &h);
        assert_eq!(h.len(), 2, "expected a move, got {h:?}");
        assert!(
            h.iter()
                .all(|e| e.old_range().is_empty() || e.new_range().is_empty()),
            "a move is a delete and an insert, not a replace: {h:?}"
        );
    }

    #[test]
    fn patience_hands_a_region_of_repeats_to_myers() {
        // Every line appears twice, so patience has no anchor it will accept and
        // falls through — which must be *exactly* myers, not an approximation of
        // it. Histogram is free to anchor here and does.
        let old = lines("a\nb\na\nb\n");
        let new = lines("b\na\nb\na\n");
        let p = Patience::default().diff("x", &old, &new);
        verify(&old, &new, &p);
        assert_eq!(
            p,
            Myers::default().diff("x", &old, &new),
            "the fallback is not myers"
        );
        let h = Histogram::default().diff("x", &old, &new);
        verify(&old, &new, &h);
    }

    #[test]
    fn an_exhausted_budget_degrades_to_a_replace_instead_of_stalling() {
        // What a fully rewritten generated file hits. Driven through `Ctx`
        // directly with a small budget rather than by feeding it 60k lines,
        // because the cost of reaching `MAX_STEPS` honestly *is* MAX_STEPS and
        // `cargo test -p gitten-core` is meant to stay under a second.
        let old: Vec<Arc<str>> = (0..400).map(|i| Arc::from(format!("old {i}"))).collect();
        let new: Vec<Arc<str>> = (0..400).map(|i| Arc::from(format!("new {i}"))).collect();
        let (a, b) = intern(&old, &new);

        let mut ctx = Ctx {
            steps: 50,
            ..Default::default()
        };
        let mut out = Vec::new();
        ctx.myers(&a, &b, Region::whole(&a, &b), &mut out);
        verify(&old, &new, &out);
        assert_eq!(
            out,
            vec![Edit {
                old_start: 0,
                old_end: 400,
                new_start: 0,
                new_end: 400
            }],
            "an exhausted myers must say the region was replaced"
        );

        // The anchored ones charge their index build to the same budget, and a
        // region they cannot afford to index goes to myers, which cannot afford
        // it either — so the same replace comes out.
        let mut ctx = Ctx {
            steps: 50,
            ..Default::default()
        };
        let mut out = Vec::new();
        ctx.anchored(&a, &b, MAX_ANCHOR_OCCURRENCES, &mut out);
        verify(&old, &new, &out);
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn histogram_scores_a_run_by_its_rarest_line() {
        // `fn anchor`'s doc says it plainly: a run is scored by its *rarest*
        // line, not its most common one. Get that backwards and a long run of
        // unique code loses to a short one the moment a common line like `}`
        // falls inside it — the same class of bug that cost 582 spurious
        // changed-line pairs on this repository's own history.
        //
        // Driven through `Ctx::anchor` directly, as `an_exhausted_budget_...`
        // above does: asserting on the final `Differ::diff` script does not
        // pin this, because the recursion can absorb a wrong top-level anchor
        // back into the same edit script on inputs this small.
        //
        // Five-line run at the front, identical on both sides, with a `}` as
        // its third line. `}` is padded to five occurrences total, but only in
        // `old` — enough to inflate its global count without giving the scan
        // anything else to trip over. A two-line run further along, also
        // identical on both sides and made entirely of lines that appear
        // nowhere else, is the only other candidate.
        let old: Vec<Arc<str>> = [
            "run_u0", "run_u1", "}", "run_u2", "run_u3", // the long run: 0..5
            "a_only_1", "}", "a_only_2", "}", "a_only_3", "}", "a_only_4",
            "}", // padding, 5..13
            "mid_a_1", "mid_a_2", // 13..15
            "run_s0", "run_s1", // the short run: 15..17
            "a_tail_1", "a_tail_2", // 17..19
        ]
        .into_iter()
        .map(Arc::from)
        .collect();
        let new: Vec<Arc<str>> = [
            "run_u0", "run_u1", "}", "run_u2", "run_u3", // the long run: 0..5
            "b_only_1", "b_only_2", "b_only_3", "b_only_4", // 5..9
            "b_only_5", "b_only_6", "b_only_7", "b_only_8", // 9..13
            "mid_b_1", "mid_b_2", // 13..15
            "run_s0", "run_s1", // the short run: 15..17
            "b_tail_1", "b_tail_2", // 17..19
        ]
        .into_iter()
        .map(Arc::from)
        .collect();
        let (a, b) = intern(&old, &new);

        let mut ctx = Ctx::default();
        ctx.begin_file();
        match ctx.anchor(&a, &b, Region::whole(&a, &b), MAX_ANCHOR_OCCURRENCES) {
            Anchor::At { a_at, b_at, len } => {
                assert_eq!(
                    (a_at, b_at, len),
                    (0, 0, 5),
                    "scored by its rarest line, the five-line run must win over \
                     the two-line one, `}}` inside it or not"
                );
            }
            Anchor::TooCommon => {
                panic!("expected an anchor, budget or threshold ruled everything out")
            }
            Anchor::Disjoint => panic!("expected an anchor, the two runs are common to both sides"),
        }
    }

    #[test]
    fn an_anchor_that_peels_one_line_at_a_time_still_finishes() {
        // The shape that makes recursion depth equal to file length: a unique
        // line between every pair of identical ones, so every anchor is one line
        // long and the region to the right of it is almost the whole file. As a
        // call stack that is a crash; as a work stack it is a loop.
        let old: Vec<Arc<str>> = (0..2000)
            .flat_map(|i| [Arc::from(format!("anchor {i}")), Arc::from("x")])
            .collect();
        let new: Vec<Arc<str>> = (0..2000)
            .flat_map(|i| [Arc::from(format!("anchor {i}")), Arc::from("y")])
            .collect();
        for d in all() {
            let edits = d.diff("generated.rs", &old, &new);
            verify(&old, &new, &edits);
        }
    }

    #[test]
    fn a_region_with_nothing_in_common_is_one_replace() {
        let old = lines("aaa\nbbb\n");
        let new = lines("ccc\nddd\n");
        for d in all() {
            let edits = d.diff("x", &old, &new);
            verify(&old, &new, &edits);
            assert_eq!(
                edits,
                vec![Edit {
                    old_start: 0,
                    old_end: 2,
                    new_start: 0,
                    new_end: 2
                }],
                "{}",
                d.name()
            );
        }
    }

    #[test]
    fn a_repeating_file_does_not_overflow_the_stack() {
        // The shape that makes recursion depth equal to file length: every
        // anchor peels one line. 40k lines of it, which as a call stack is a
        // crash rather than a slow load.
        let old: Vec<Arc<str>> = (0..40_000)
            .map(|i| Arc::from(format!("line {}", i % 3)))
            .collect();
        let new: Vec<Arc<str>> = (0..40_000)
            .map(|i| Arc::from(format!("line {}", i % 4)))
            .collect();
        for d in all() {
            let edits = d.diff("x", &old, &new);
            verify(&old, &new, &edits);
        }
    }

    #[test]
    fn a_fully_rewritten_generated_file_degrades_instead_of_stalling() {
        // 60k lines against 60k different lines is 10^10 Myers steps. The budget
        // has to turn that into a replace, and the result still has to apply.
        let old: Vec<Arc<str>> = (0..60_000).map(|i| Arc::from(format!("old {i}"))).collect();
        let new: Vec<Arc<str>> = (0..60_000).map(|i| Arc::from(format!("new {i}"))).collect();
        let t = std::time::Instant::now();
        let edits = Myers::default().diff("bundle.js", &old, &new);
        verify(&old, &new, &edits);
        assert!(t.elapsed().as_secs() < 20, "took {:?}", t.elapsed());
    }

    // ------------------------------------------------------------ hunk output

    #[test]
    fn hunks_carry_context_and_both_line_numbers() {
        let old = lines("1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
        let new = lines("1\n2\n3\n4\nFIVE\n6\n7\n8\n9\n10\n");
        let edits = Histogram::default().diff("x", &old, &new);
        let hs = hunks(&old, &new, &edits, 3);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].header, "@@ -2,7 +2,7 @@");
        let kinds: Vec<LineKind> = hs[0].lines.iter().map(|l| l.kind).collect();
        use LineKind::*;
        assert_eq!(
            kinds,
            vec![Context, Context, Context, Removed, Added, Context, Context, Context]
        );
        // Numbers are 1-based and both sides advance over context.
        assert_eq!(
            (hs[0].lines[0].old_no, hs[0].lines[0].new_no),
            (Some(2), Some(2))
        );
        let removed = hs[0].lines.iter().find(|l| l.kind == Removed).unwrap();
        assert_eq!((removed.old_no, removed.new_no), (Some(5), None));
        let added = hs[0].lines.iter().find(|l| l.kind == Added).unwrap();
        assert_eq!((added.old_no, added.new_no), (None, Some(5)));
    }

    #[test]
    fn nearby_changes_share_a_hunk_and_distant_ones_do_not() {
        let old: Vec<Arc<str>> = (1..=40).map(|i| Arc::from(i.to_string())).collect();
        let mut new = old.clone();
        new[5] = "six".into();
        new[10] = "eleven".into(); // 5 lines away: context overlaps
        new[30] = "thirtyone".into(); // far away
        let hs = hunks(&old, &new, &Histogram::default().diff("x", &old, &new), 3);
        assert_eq!(
            hs.len(),
            2,
            "{:?}",
            hs.iter().map(|h| &h.header).collect::<Vec<_>>()
        );
        // No line is printed twice: the merged hunk covers 3..14, the other 28..34.
        assert!(
            hs[0]
                .lines
                .iter()
                .filter(|l| l.kind == LineKind::Removed)
                .count()
                == 2
        );
    }

    #[test]
    fn context_of_zero_prints_only_the_change() {
        let old = lines("a\nb\nc\n");
        let new = lines("a\nB\nc\n");
        let hs = hunks(&old, &new, &Histogram::default().diff("x", &old, &new), 0);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].lines.len(), 2);
        assert_eq!(
            hs[0].header, "@@ -2 +2 @@ a",
            "a count of one is written without it"
        );
    }

    #[test]
    fn a_pure_insertion_names_the_line_before_it() {
        // git's convention, and it looks like an off-by-one until you know it:
        // an empty old side prints the line the insertion comes *after*.
        let old = lines("a\n");
        let new = lines("a\nb\n");
        let hs = hunks(&old, &new, &Myers::default().diff("x", &old, &new), 0);
        assert_eq!(hs[0].header, "@@ -1,0 +2 @@ a");

        let hs = hunks(&new, &old, &Myers::default().diff("x", &new, &old), 0);
        assert_eq!(hs[0].header, "@@ -2 +1,0 @@ a");
    }

    #[test]
    fn the_header_names_the_enclosing_declaration() {
        let old = lines("fn dispatch() {\n    a();\n    b();\n    c();\n}\n");
        let new = lines("fn dispatch() {\n    a();\n    B();\n    c();\n}\n");
        let hs = hunks(
            &old,
            &new,
            &Histogram::default().diff("x.rs", &old, &new),
            1,
        );
        assert_eq!(hs[0].header, "@@ -2,3 +2,3 @@ fn dispatch() {");
    }

    #[test]
    fn the_header_search_gives_up_rather_than_scanning_a_whole_file() {
        // A formatted blob: every line indented, so there is no declaration to
        // find and the search would otherwise walk to line 0 for every hunk.
        let mut old: Vec<Arc<str>> = (0..2000)
            .map(|i| Arc::from(format!("    \"k{i}\": {i},")))
            .collect();
        old[0] = "root = {".into();
        let mut new = old.clone();
        new[1500] = "    \"k1500\": 99,".into();
        let hs = hunks(
            &old,
            &new,
            &Histogram::default().diff("x.json", &old, &new),
            3,
        );
        assert_eq!(hs.len(), 1);
        assert_eq!(
            hs[0].header, "@@ -1498,7 +1498,7 @@",
            "no declaration within reach"
        );

        // Within reach, it is still found.
        let hs = hunks(
            &old[1200..],
            &new[1200..],
            &Myers::default().diff("x", &old[1200..], &new[1200..]),
            0,
        );
        assert!(hs[0].header.ends_with("@@"), "{}", hs[0].header);
    }

    #[test]
    fn a_hunk_at_the_very_start_or_end_does_not_run_off_the_file() {
        let old = lines("a\nb\n");
        let new = lines("A\nb\n");
        let hs = hunks(&old, &new, &Myers::default().diff("x", &old, &new), 10);
        assert_eq!(hs[0].header, "@@ -1,2 +1,2 @@");
        assert_eq!(hs[0].lines.len(), 3);

        let old = lines("a\nb\n");
        let new = lines("a\nB\n");
        let hs = hunks(&old, &new, &Myers::default().diff("x", &old, &new), 10);
        assert_eq!(hs[0].header, "@@ -1,2 +1,2 @@");
    }

    #[test]
    fn no_edits_is_no_hunks() {
        let f = lines("a\nb\n");
        assert!(hunks(&f, &f, &[], 3).is_empty());
    }

    #[test]
    fn every_hunk_line_number_matches_the_file_it_came_from() {
        // The property that makes a header trustworthy, over a diff big enough
        // that an off-by-one somewhere would not be visible by reading.
        let old: Vec<Arc<str>> = (0..500).map(|i| Arc::from(format!("line {i}"))).collect();
        let new: Vec<Arc<str>> = (0..500)
            .filter(|i| i % 7 != 0)
            .map(|i| {
                if i % 11 == 0 {
                    Arc::from(format!("changed {i}"))
                } else {
                    Arc::from(format!("line {i}"))
                }
            })
            .collect();
        for d in all() {
            let edits = d.diff("x", &old, &new);
            verify(&old, &new, &edits);
            for h in hunks(&old, &new, &edits, 3) {
                for l in &h.lines {
                    if let Some(no) = l.old_no {
                        assert_eq!(
                            &*old[no as usize - 1],
                            &*l.text,
                            "{} old line {no}",
                            d.name()
                        );
                    }
                    if let Some(no) = l.new_no {
                        assert_eq!(
                            &*new[no as usize - 1],
                            &*l.text,
                            "{} new line {no}",
                            d.name()
                        );
                    }
                    if l.kind == LineKind::Context {
                        assert!(l.old_no.is_some() && l.new_no.is_some());
                    }
                }
            }
        }
    }

    // -------------------------------------------------------------- registry

    struct Reverse;
    impl Differ for Reverse {
        fn name(&self) -> &'static str {
            "reverse"
        }
        fn diff(&self, _path: &str, old: &[Arc<str>], new: &[Arc<str>]) -> Vec<Edit> {
            vec![Edit {
                old_start: 0,
                old_end: old.len() as u32,
                new_start: 0,
                new_end: new.len() as u32,
            }]
        }
    }

    #[test]
    fn the_shipped_registry_selects_histogram() {
        let d = Differs::builtin();
        assert_eq!(d.selected(), "histogram");
        assert_eq!(d.names(), vec!["histogram", "patience", "myers"]);
        assert_eq!(d.context, 3);
        assert_eq!(d.for_path("a.rs").name(), "histogram");
    }

    #[test]
    fn an_algorithm_can_be_added_selected_and_routed() {
        // The whole of what an extension does, and the proof the seam is real.
        let mut d = Differs::builtin();
        assert!(!d.select("reverse"), "not registered yet");
        d.register(Reverse);
        assert!(d.select("reverse"));
        assert_eq!(d.for_path("a.rs").name(), "reverse");
        assert!(d.route(&["rs", "Cargo.lock"], "myers"));
        assert_eq!(d.for_path("a.rs").name(), "myers");
        assert_eq!(d.for_path("deep/path/cargo.lock").name(), "myers");
        assert_eq!(
            d.for_path("a.go").name(),
            "reverse",
            "unrouted paths keep the fallback"
        );
        assert!(!d.route(&["go"], "nope"));
    }

    #[test]
    fn registering_a_name_twice_replaces_it() {
        struct FakeMyers;
        impl Differ for FakeMyers {
            fn name(&self) -> &'static str {
                "myers"
            }
            fn diff(&self, _: &str, _: &[Arc<str>], _: &[Arc<str>]) -> Vec<Edit> {
                Vec::new()
            }
        }
        let mut d = Differs::builtin();
        d.register(FakeMyers);
        assert_eq!(d.names().len(), 3, "a replacement must not also append");
        assert!(d.select("myers"));
        let f = lines("a\nb\n");
        assert!(
            d.for_path("x").diff("x", &f, &[]).is_empty(),
            "the built-in still ran"
        );
        // ...and the one that was not replaced is untouched.
        assert!(d.select("histogram"));
        assert!(!d.for_path("x").diff("x", &f, &[]).is_empty());
    }

    #[test]
    fn a_file_goes_through_the_registry_in_one_call() {
        let old = lines("a\nb\nc\n");
        let new = lines("a\nB\nc\n");
        let mut d = Differs::builtin();
        d.context = 1;
        let f = d.file("src/x.rs", &old, &new);
        assert_eq!(f.path, "src/x.rs");
        assert_eq!(f.hunks.len(), 1);
        assert_eq!(f.hunks[0].lines.len(), 4);
    }

    #[test]
    fn a_runtime_override_beats_both_the_routes_and_the_fallback() {
        // What the title-bar dropdown does. The override has to win over a
        // route as well, or the control says "myers" while some paths are still
        // on something else.
        let mut d = Differs::builtin();
        d.register(Reverse);
        assert!(d.route(&["rs"], "reverse"));
        let old = lines("a\nb\nc\n");
        let new = lines("a\nB\nc\n");

        assert_eq!(d.for_path("x.rs").name(), "reverse");
        let routed = d.file("x.rs", &old, &new);
        let overridden = d.file_using(&Overrides::algorithm("myers"), "x.rs", &old, &new, None);
        assert_ne!(
            routed.hunks[0].lines.len(),
            overridden.hunks[0].lines.len(),
            "the override did not beat the route"
        );

        // An unregistered name is a click that cannot be honoured, so it falls
        // back rather than producing nothing.
        assert_eq!(
            d.file_using(&Overrides::algorithm("nope"), "x.rs", &old, &new, None),
            routed
        );
        assert_eq!(
            d.file_using(&Overrides::default(), "x.rs", &old, &new, None),
            routed
        );
    }

    #[test]
    fn an_implementation_is_reachable_by_name() {
        let mut d = Differs::builtin();
        d.register(Reverse);
        assert_eq!(d.by_name("reverse").map(|x| x.name()), Some("reverse"));
        assert_eq!(d.by_name("histogram").map(|x| x.name()), Some("histogram"));
        assert!(d.by_name("nope").is_none());
    }

    // ------------------------------------------------------------ whitespace

    #[test]
    fn each_whitespace_relation_matches_gits_definition() {
        let key = |w: Whitespace, l: &str| w.key(l).unwrap_or_else(|| l.to_string());

        // `Exact` is byte for byte and allocates nothing.
        assert!(Whitespace::Exact.key("  a  ").is_none());

        // `--ignore-space-at-eol`: the end only.
        assert_eq!(key(Whitespace::Trailing, "  a  b   "), "  a  b");
        assert_ne!(
            key(Whitespace::Trailing, "  a"),
            key(Whitespace::Trailing, "    a"),
            "leading indent still counts"
        );

        // `-b`: any run of whitespace equals any other, trailing goes. A leading
        // run collapses to one space rather than vanishing, so an indented line
        // and a flush one still differ — that is git's rule and the easy one to
        // get wrong.
        assert_eq!(key(Whitespace::Change, "a\t \tb  "), "a b");
        assert_eq!(
            key(Whitespace::Change, "  a b"),
            key(Whitespace::Change, "\ta\tb")
        );
        assert_ne!(key(Whitespace::Change, "a"), key(Whitespace::Change, " a"));

        // `-w`: all of it, anywhere.
        assert_eq!(key(Whitespace::All, "  a \t b  "), "ab");
        assert_eq!(key(Whitespace::All, "ab"), key(Whitespace::All, " a b "));

        // A line of nothing but whitespace is blank under every relation but
        // exact, which is what makes `git -w` treat a reindent as no change.
        for w in [Whitespace::Trailing, Whitespace::Change, Whitespace::All] {
            assert_eq!(key(w, "   \t "), "", "{}", w.name());
        }
    }

    #[test]
    fn ignoring_whitespace_makes_a_reindent_no_change_at_all() {
        // The reason anybody reaches for `-w`. Same code, two spaces to four.
        let old = lines("fn main() {\n  let x = 1;\n  f(x);\n}\n");
        let new = lines("fn main() {\n    let x = 1;\n    f(x);\n}\n");

        let mut d = Differs::builtin();
        assert!(
            !d.file("a.rs", &old, &new).hunks.is_empty(),
            "exact must see it"
        );

        d.whitespace = Whitespace::All;
        assert!(d.file("a.rs", &old, &new).hunks.is_empty(), "-w must not");
        d.whitespace = Whitespace::Change;
        assert!(
            d.file("a.rs", &old, &new).hunks.is_empty(),
            "-b must not either"
        );
        // ...and `--ignore-space-at-eol` must, because this is leading space.
        d.whitespace = Whitespace::Trailing;
        assert!(!d.file("a.rs", &old, &new).hunks.is_empty());
    }

    #[test]
    fn a_whitespace_only_change_still_shows_the_real_text() {
        // The property that makes normalising safe: it is for comparison only.
        // The hunk shows the file's actual bytes, and the line numbers address
        // the actual lines.
        let old = lines("keep\n  a\nreal change\n");
        let new = lines("keep\n    a\nreal changed\n");
        let mut d = Differs::builtin();
        d.whitespace = Whitespace::All;
        d.context = 3;
        let f = d.file("a.rs", &old, &new);
        for l in f.hunks.iter().flat_map(|h| &h.lines) {
            // Every line's text is a real line of a real file, never a key. A
            // context line comes from the *old* side, which is why its `new_no`
            // may point at bytes that differ — that is `-w` working, and it is
            // what git prints too.
            match l.kind {
                LineKind::Removed | LineKind::Context => {
                    let n = l.old_no.expect("has an old line") as usize;
                    assert_eq!(&*old[n - 1], &*l.text, "old line {n} was normalised");
                }
                LineKind::Added => {
                    let n = l.new_no.expect("has a new line") as usize;
                    assert_eq!(&*new[n - 1], &*l.text, "new line {n} was normalised");
                }
            }
        }
        // The reindented line is context, shown as the old file had it.
        let context: Vec<&str> = f.hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Context)
            .map(|l| l.text.as_ref())
            .collect();
        assert!(context.contains(&"  a"), "{context:?}");
    }

    #[test]
    fn a_whitespace_relation_composes_with_every_algorithm() {
        // The reason this is a knob and not three more implementations: it has to
        // work for an extension's differ too, which cannot know it exists.
        let old = lines("a\n  b\nc\n");
        let new = lines("a\n    b\nc\n");
        for name in ["histogram", "patience", "myers"] {
            let mut d = Differs::builtin();
            assert!(d.select(name));
            d.whitespace = Whitespace::All;
            assert!(d.file("x", &old, &new).hunks.is_empty(), "{name}");
        }
    }

    // ---------------------------------------------------------------- moves

    #[test]
    fn a_block_moved_across_a_file_is_reported_as_moved() {
        let old = lines(
            "fn a() {\n    one();\n    two();\n    three();\n}\n\nfn b() {\n    keep();\n}\n",
        );
        let new = lines(
            "fn b() {\n    keep();\n}\n\nfn a() {\n    one();\n    two();\n    three();\n}\n",
        );
        let d = Differs::builtin();
        let f = d.file("x.rs", &old, &new);
        let lines_of = |kind: LineKind| -> Vec<&str> {
            f.hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| l.moved && l.kind == kind)
                .map(|l| l.text.as_ref())
                .collect()
        };
        // *Which* block is called the mover is the differ's choice — moving the
        // shorter one is the smaller script, and either reading is true. What
        // must hold is that both halves are marked and they are the same text.
        let removed = lines_of(LineKind::Removed);
        let added = lines_of(LineKind::Added);
        assert!(!removed.is_empty(), "no removed line was called moved");
        assert_eq!(
            removed, added,
            "the two halves of a move must be the same lines"
        );
        assert!(removed.len() >= MIN_MOVED_LINES);
        assert!(
            f.hunks
                .iter()
                .flat_map(|h| &h.lines)
                .all(|l| !l.moved || l.kind != LineKind::Context),
            "a context line was called moved"
        );
    }

    #[test]
    fn two_matching_lines_are_a_coincidence_and_not_a_move() {
        // `}` and a blank line are everywhere. Reporting them costs the feature
        // its whole value, which is that a moved block can be skipped.
        let old = lines("a\n}\nb\n");
        let new = lines("b\n}\na\n");
        let d = Differs::builtin();
        let f = d.file("x.rs", &old, &new);
        assert!(
            f.hunks.iter().flat_map(|h| &h.lines).all(|l| !l.moved),
            "{f:?}"
        );
    }

    #[test]
    fn move_detection_can_be_turned_off() {
        // Five lines and three: moving the three is the smaller script, so that
        // is what the differ does and what there is to detect.
        let old = lines("k1\nk2\nk3\nk4\nk5\nm1\nm2\nm3\n");
        let new = lines("m1\nm2\nm3\nk1\nk2\nk3\nk4\nk5\n");
        let mut d = Differs::builtin();
        assert!(d
            .file("x", &old, &new)
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| l.moved));
        d.min_moved = 0;
        assert!(d
            .file("x", &old, &new)
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .all(|l| !l.moved));
    }

    #[test]
    fn a_block_deleted_once_and_added_twice_claims_one_landing() {
        // Driven through `moves` with a hand-written script rather than a differ,
        // because no differ would produce this shape and the property is still
        // worth pinning: without the `taken` bitmap the same three removed lines
        // mark six added ones, and the moved count exceeds what exists.
        let old = lines("one\ntwo\nthree\n");
        let new = lines("one\ntwo\nthree\none\ntwo\nthree\n");
        let edits = vec![Edit {
            old_start: 0,
            old_end: 3,
            new_start: 0,
            new_end: 6,
        }];
        let m = moves(&old, &new, &edits, 3);
        assert_eq!(m.old.iter().filter(|b| **b).count(), 3);
        assert_eq!(
            m.new.iter().filter(|b| **b).count(),
            3,
            "two landings were claimed"
        );
    }

    #[test]
    fn moves_ignores_blank_lines_when_matching() {
        // A run of blank lines is not a moved block, however long it is.
        let old = lines("a\n\n\n\n\nb\n");
        let new = lines("b\n\n\n\n\na\n");
        let edits = vec![Edit {
            old_start: 0,
            old_end: old.len() as u32,
            new_start: 0,
            new_end: new.len() as u32,
        }];
        assert!(moves(&old, &new, &edits, 3).is_empty());
    }

    #[test]
    fn moves_never_claims_a_line_the_script_did_not_touch() {
        // The bug this guards: indexing over the whole file rather than the
        // changed lines makes every repeated line in an unchanged region a move.
        let old = lines("a\nb\nc\na\nb\nc\nchanged\n");
        let new = lines("a\nb\nc\na\nb\nc\nCHANGED\n");
        let d = Differs::builtin();
        let f = d.file("x", &old, &new);
        assert!(
            f.hunks.iter().flat_map(|h| &h.lines).all(|l| !l.moved),
            "{f:?}"
        );
    }

    // ------------------------------------------------------ indent heuristic

    #[test]
    fn a_hunk_slides_to_the_readable_boundary() {
        // The canonical case, and the one every diff tool is judged on: a
        // function added before an existing one. Without the heuristic the change
        // is attributed starting at the *previous* function's closing brace, so
        // the hunk reads as "} + fn b() {" instead of "fn a() { ... }".
        let old = lines("fn b() {\n    b();\n}\n");
        let new = lines("fn a() {\n    a();\n}\n\nfn b() {\n    b();\n}\n");

        let mut d = Differs::builtin();
        d.context = 0;
        let with = d.file("x.rs", &old, &new);
        d.indent_heuristic = false;
        let without = d.file("x.rs", &old, &new);

        // Same amount of change either way — a slide is not a better diff, it is
        // the same diff in a more readable place.
        let count = |f: &FileDiff| {
            f.hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| l.kind != LineKind::Context)
                .count()
        };
        assert_eq!(count(&with), count(&without));

        let added = |f: &FileDiff| -> Vec<String> {
            f.hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| l.kind == LineKind::Added)
                .map(|l| l.text.to_string())
                .collect()
        };
        assert_eq!(
            added(&with),
            vec!["fn a() {", "    a();", "}", ""],
            "the added block should be the new function and the blank after it"
        );
        // Not asserted to *differ* from the unslid answer: on this input the
        // differ already puts the group in the right place. That the heuristic
        // does something is what `git/examples/diffcheck.rs` measures, by
        // comparing every hunk position against git's — five of its six rows
        // match exactly, which is a stronger statement than any single case.
        let _ = added(&without);
    }

    #[test]
    fn sliding_never_changes_what_the_diff_says() {
        // The invariant: compaction moves a group, it never resizes one, and the
        // script must still apply. Over the awkward shapes.
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut rand = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let pool = [
            "",
            "a",
            "    a",
            "        b",
            "}",
            "    }",
            "fn f() {",
            "  # c",
        ];
        for case in 0..300 {
            let (n, m) = (rand(20) as usize, rand(20) as usize);
            let old: Vec<Arc<str>> = (0..n).map(|_| Arc::from(pool[rand(8) as usize])).collect();
            let new: Vec<Arc<str>> = (0..m).map(|_| Arc::from(pool[rand(8) as usize])).collect();
            let mut edits = Histogram::default().diff("x.rs", &old, &new);
            let before = changed(&edits);
            compact(&old, &new, &mut edits);
            verify(&old, &new, &edits);
            assert_eq!(changed(&edits), before, "case {case} resized a group");
        }
    }

    #[test]
    fn a_slide_uses_the_whitespace_relation_to_decide_it_can_move() {
        // Whether a group *can* slide is the relation's question; how readable the
        // result is, is the text's. Conflating them loses a slide git makes —
        // under `-w` a group may cross a line that differs from it only in
        // indentation, and comparing the real text says it may not. Two hunks in
        // cmux's history land in the wrong place without this.
        let old: Vec<Arc<str>> = ["a();", "  x();", "b();"]
            .iter()
            .copied()
            .map(Arc::from)
            .collect();
        let new: Vec<Arc<str>> = ["a();", "x();", "  x();", "b();"]
            .iter()
            .copied()
            .map(Arc::from)
            .collect();
        let strip = |l: &Arc<str>| -> Arc<str> {
            Arc::from(l.chars().filter(|c| !c.is_whitespace()).collect::<String>())
        };
        let (ok, nk): (Vec<Arc<str>>, Vec<Arc<str>>) = (
            old.iter().map(strip).collect(),
            new.iter().map(strip).collect(),
        );

        let script = Histogram::default().diff("x.rs", &ok, &nk);
        let mut through_keys = script.clone();
        let mut through_text = script.clone();
        compact_with(&old, &new, &ok, &nk, &mut through_keys);
        compact(&old, &new, &mut through_text);
        verify(&ok, &nk, &through_keys);
        verify(&ok, &nk, &through_text);
        assert_ne!(
            through_keys, through_text,
            "the relation made no difference to where the group could go: {script:?}"
        );
    }

    #[test]
    fn a_slide_cannot_step_over_a_neighbouring_change() {
        // Two changes with one line between them: sliding either onto the other
        // would produce overlapping edits, which is the one thing `verify`
        // catches and a reader never would.
        let old: Vec<Arc<str>> = (0..40)
            .map(|i| Arc::from(format!("    line {i}")))
            .collect();
        let mut new = old.clone();
        new.insert(10, "    inserted a".into());
        new.insert(12, "    inserted b".into());
        let mut edits = Histogram::default().diff("x.rs", &old, &new);
        compact(&old, &new, &mut edits);
        verify(&old, &new, &edits);
    }

    #[test]
    fn the_context_setting_reaches_the_hunks() {
        let old: Vec<Arc<str>> = (0..20).map(|i| Arc::from(i.to_string())).collect();
        let mut new = old.clone();
        new[10] = "ten!".into();
        for context in [0, 1, 3, 7] {
            let mut d = Differs::builtin();
            d.context = context;
            let f = d.file("x", &old, &new);
            assert_eq!(f.hunks[0].lines.len(), 2 + 2 * context, "context {context}");
        }
    }

    // ---------------------------------------------------------------- cache

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts invocations and answers one whole-file replace — the same
    /// instrument the acquisition tests use, because the property under test
    /// is "how many times did the differ run", not what it says. Named, so a
    /// registry can hold two of them and an algorithm override can be
    /// observed moving work from one to the other.
    struct Counting(Arc<AtomicUsize>, &'static str);

    impl Differ for Counting {
        fn name(&self) -> &'static str {
            self.1
        }
        fn diff(&self, _path: &str, old: &[Arc<str>], new: &[Arc<str>]) -> Vec<Edit> {
            self.0.fetch_add(1, Ordering::Relaxed);
            vec![Edit {
                old_start: 0,
                old_end: old.len() as u32,
                new_start: 0,
                new_end: new.len() as u32,
            }]
        }
    }

    fn counted() -> (Differs, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut d = Differs::builtin();
        d.register(Counting(Arc::clone(&calls), "counting"));
        assert!(d.select("counting"));
        (d, calls)
    }

    fn calls(c: &Arc<AtomicUsize>) -> usize {
        c.load(Ordering::Relaxed)
    }

    fn sample() -> (Vec<Arc<str>>, Vec<Arc<str>>) {
        let old: Vec<Arc<str>> = (0..12).map(|i| Arc::from(format!("line {i}"))).collect();
        let mut new = old.clone();
        new[4] = "four!".into();
        (old, new)
    }

    const OIDS: (&str, &str) = ("aaaa", "bbbb");

    #[test]
    fn a_second_identical_call_is_a_hit_and_the_differ_never_runs() {
        let (d, count) = counted();
        let (old, new) = sample();

        let first = d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));
        assert_eq!(calls(&count), 1);

        // Different line handles, same identity: the whole point is that a
        // re-acquisition builds fresh `Pair`s whose text happens to be equal.
        let (old2, new2) = sample();
        let second = d.file_using(&Overrides::default(), "x.rs", &old2, &new2, Some(OIDS));
        assert_eq!(calls(&count), 1, "a hit must not reach the differ");
        assert_eq!(first, second, "a hit must be byte-identical to a miss");
    }

    #[test]
    fn every_setting_that_reaches_the_answer_is_in_the_key() {
        let (mut d, count) = counted();
        let (old, new) = sample();
        d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));
        assert_eq!(calls(&count), 1);

        // Context shapes the hunks out of the same script.
        d.context = 7;
        d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));
        assert_eq!(calls(&count), 2, "a context change must miss");

        // So does the move floor, even when the script has no moves.
        d.min_moved = 0;
        d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));
        assert_eq!(calls(&count), 3, "a min_moved change must miss");

        // And the indent heuristic, which slides the script.
        d.indent_heuristic = false;
        d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));
        assert_eq!(calls(&count), 4, "an indent-heuristic change must miss");

        // The algorithm override resolves before the key is built: a second
        // named counter takes the work, and the fallback's counter does not
        // move — the miss is visible where the computation landed.
        let other = Arc::new(AtomicUsize::new(0));
        d.register(Counting(Arc::clone(&other), "counting2"));
        d.file_using(
            &Overrides::algorithm("counting2"),
            "x.rs",
            &old,
            &new,
            Some(OIDS),
        );
        assert_eq!(calls(&other), 1, "the overridden algorithm ran");
        assert_eq!(calls(&count), 4, "the fallback did not");

        // The whitespace relation changes what the differ compares.
        d.file_using(
            &Overrides {
                whitespace: Some(Whitespace::All),
                ..Default::default()
            },
            "x.rs",
            &old,
            &new,
            Some(OIDS),
        );
        // Four fallback misses so far; the override above computed elsewhere.
        assert_eq!(calls(&count), 5, "a whitespace change must miss");
    }

    #[test]
    fn an_oid_change_misses_even_when_no_setting_did() {
        let (d, count) = counted();
        let (old, new) = sample();
        d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));
        d.file_using(
            &Overrides::default(),
            "x.rs",
            &old,
            &new,
            Some(("cccc", "dddd")),
        );
        assert_eq!(calls(&count), 2, "different blobs are different answers");
    }

    #[test]
    fn no_identity_means_always_compute() {
        let (d, count) = counted();
        let (old, new) = sample();
        for _ in 0..3 {
            d.file_using(&Overrides::default(), "x.rs", &old, &new, None);
        }
        assert_eq!(
            calls(&count),
            3,
            "None is bypass: an untracked or half-known pair is never cached"
        );
    }

    #[test]
    fn the_path_is_not_in_the_key_but_the_resolved_algorithm_is() {
        // Same blobs, same routed algorithm, two names: genuinely the same
        // hunks — headers and moves come from content alone — so one entry
        // serves both. A rename that crossed into a differently-routed
        // extension would change the resolved algorithm and miss instead.
        let (d, count) = counted();
        let (old, new) = sample();
        let a = d.file_using(&Overrides::default(), "a.txt", &old, &new, Some(OIDS));
        let b = d.file_using(&Overrides::default(), "b.txt", &old, &new, Some(OIDS));
        assert_eq!(calls(&count), 1);
        assert_eq!(a.hunks, b.hunks);
        assert_eq!(d.cache.lock().unwrap().answers.len(), 1);

        // Routing `.rs` elsewhere makes the same blobs a different answer.
        let mut routed = Differs::builtin();
        routed.register(Counting(Arc::clone(&count), "counting"));
        assert!(routed.route(&["rs"], "counting"));
        routed.select("myers");
        routed.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));
        assert_eq!(calls(&count), 2, "the route changed the resolved algorithm");
    }

    #[test]
    fn a_hit_matches_a_fresh_registry_byte_for_byte() {
        let (d, _) = counted();
        let (old, new) = sample();
        let cached = d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));

        let (fresh, fresh_count) = counted();
        let direct = fresh.file_using(&Overrides::default(), "x.rs", &old, &new, None);
        assert_eq!(calls(&fresh_count), 1, "the comparison registry computed");
        assert_eq!(cached, direct);
    }

    #[test]
    fn the_cap_evicts_oldest_first_and_only_when_crossed() {
        let (d, count) = counted();
        let (old, new) = sample();
        let oids = |n: usize| (format!("old-{n:06}"), format!("new-{n:06}"));
        let ask = |d: &Differs, n: usize| {
            let (o, w) = oids(n);
            d.file_using(
                &Overrides::default(),
                "x.rs",
                &old,
                &new,
                Some((o.as_str(), w.as_str())),
            )
        };

        // One past the cap: exactly the cap survives, and the first key in is
        // the first key out.
        for n in 0..=CACHE_CAP {
            ask(&d, n);
        }
        {
            let cache = d.cache.lock().unwrap();
            assert_eq!(cache.answers.len(), CACHE_CAP);
            assert_eq!(cache.order.len(), CACHE_CAP);
        }
        ask(&d, 0);
        assert_eq!(
            calls(&count),
            CACHE_CAP + 2,
            "the oldest entry was evicted, so it computes again"
        );
        // The newest survivor still hits.
        ask(&d, CACHE_CAP);
        assert_eq!(calls(&count), CACHE_CAP + 2);
    }

    #[test]
    fn two_threads_racing_one_key_agree_and_later_calls_hit() {
        use std::thread;

        let counter = Arc::new(AtomicUsize::new(0));
        let mut registry = Differs::builtin();
        registry.register(Counting(Arc::clone(&counter), "counting"));
        assert!(registry.select("counting"));
        let d = Arc::new(registry);
        let (old, new) = sample();

        // Two cold misses at once. The computation runs outside the lock, so
        // both may compute; determinism makes the answers identical and `put`
        // keeps whichever landed first. Serialising the computation behind the
        // lock instead would trade this rare duplicated diff for always
        // making every pane's refresh wait on every other's — not a trade.
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let d = Arc::clone(&d);
                let (old, new) = (old.clone(), new.clone());
                thread::spawn(move || {
                    d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS))
                })
            })
            .collect();
        let results: Vec<FileDiff> = handles
            .into_iter()
            .map(|h| h.join().expect("no panic"))
            .collect();

        assert_eq!(results[0], results[1], "racers must agree byte for byte");
        let after_race = calls(&counter);
        assert!(
            (1..=2).contains(&after_race),
            "each racer computed at most once: {after_race}"
        );

        // Whatever the race did, a winner is now the answer of record.
        d.file_using(&Overrides::default(), "x.rs", &old, &new, Some(OIDS));
        assert_eq!(calls(&counter), after_race, "settled to a hit");
    }
}
