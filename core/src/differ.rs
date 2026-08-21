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
//! plait-core` at well under a second.
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

use crate::{DiffLine, FileDiff, Hunk, LineKind};
use std::collections::HashMap;

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
    pub fn old(&self) -> std::ops::Range<usize> {
        self.old_start as usize..self.old_end as usize
    }

    pub fn new(&self) -> std::ops::Range<usize> {
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
/// The name is what a config file and a keybinding refer to, which is why it is
/// `&'static str` and part of the trait rather than a wrapper's business.
pub trait Differ {
    fn name(&self) -> &'static str;

    fn diff(&self, path: &str, old: &[&str], new: &[&str]) -> Vec<Edit>;
}

/// Every line replaced by a number, so the inner loops compare `u32`s.
///
/// Public because an implementation will want it: string comparison inside an
/// O(ND) loop is most of the runtime, and interning is the whole reason the
/// textbook algorithms are fast enough to be shipped as written.
pub fn intern<'a>(old: &[&'a str], new: &[&'a str]) -> (Vec<u32>, Vec<u32>) {
    // Keys borrow the caller's lines, so nothing is copied — only the `&str`
    // headers are hashed, and each distinct line only once.
    let mut ids: HashMap<&'a str, u32> = HashMap::with_capacity(old.len() + new.len());
    let mut number = |s: &&'a str| -> u32 {
        let next = ids.len() as u32;
        *ids.entry(s).or_insert(next)
    };
    let a = old.iter().map(&mut number).collect();
    let b = new.iter().map(&mut number).collect();
    (a, b)
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
#[derive(Debug, Clone, Copy, Default)]
pub struct Histogram;

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
#[derive(Debug, Clone, Copy, Default)]
pub struct Patience;

/// The minimal edit script, by Myers' 1986 algorithm with the linear-space
/// middle-snake refinement.
///
/// Fewest added and removed lines of any possible diff, which is what `git diff`
/// produces by default. Available because "smallest" is sometimes exactly what
/// is wanted — reviewing a whitespace change, or checking that a refactor really
/// did not touch anything else.
#[derive(Debug, Clone, Copy, Default)]
pub struct Myers;

impl Differ for Histogram {
    fn name(&self) -> &'static str {
        "histogram"
    }

    fn diff(&self, _path: &str, old: &[&str], new: &[&str]) -> Vec<Edit> {
        anchored(old, new, MAX_ANCHOR_OCCURRENCES)
    }
}

impl Differ for Patience {
    fn name(&self) -> &'static str {
        "patience"
    }

    fn diff(&self, _path: &str, old: &[&str], new: &[&str]) -> Vec<Edit> {
        anchored(old, new, 1)
    }
}

impl Differ for Myers {
    fn name(&self) -> &'static str {
        "myers"
    }

    fn diff(&self, _path: &str, old: &[&str], new: &[&str]) -> Vec<Edit> {
        let (a, b) = intern(old, new);
        let mut out = Vec::new();
        Ctx::new().myers(&a, &b, Region::whole(&a, &b), &mut out);
        out
    }
}

fn anchored(old: &[&str], new: &[&str], max_occurrences: u32) -> Vec<Edit> {
    let (a, b) = intern(old, new);
    let mut out = Vec::new();
    Ctx::new().anchored(&a, &b, max_occurrences, &mut out);
    out
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
        Self { a0: 0, a1: a.len(), b0: 0, b1: b.len() }
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

/// The step budget and the reusable occurrence index, shared across every
/// region of one file.
struct Ctx {
    steps: usize,
    /// Rebuilt per region rather than per file: rarity is only meaningful
    /// relative to the region being anchored, so a `}` that appears twice in the
    /// six lines under consideration is a usable anchor even though it appears
    /// four hundred times in the file. Git's histogram does the same.
    occurrences: HashMap<u32, Chain>,
}

/// Where a line appears in a region, and how often.
///
/// The position list is capped because the anchor search walks it per candidate;
/// the count is not, because the count is what decides whether the line is worth
/// anchoring on at all.
#[derive(Default)]
struct Chain {
    count: u32,
    at: Vec<u32>,
}

impl Ctx {
    fn new() -> Self {
        Self { steps: MAX_STEPS, occurrences: HashMap::new() }
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
        // Sized on the region, so a fallback for six anchorless lines inside a
        // large file allocates for six lines.
        let span = (region.a1 - region.a0) + (region.b1 - region.b0);
        let mut fwd = V::new(span);
        let mut bwd = V::new(span);
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
            match self.split(a, b, r, &mut fwd, &mut bwd) {
                Some((x, y)) => {
                    stack.push(Region { a0: x, a1: r.a1, b0: y, b1: r.b1 });
                    stack.push(Region { a0: r.a0, a1: x, b0: r.b0, b1: y });
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
    fn split(
        &mut self,
        a: &[u32],
        b: &[u32],
        r: Region,
        fwd: &mut V,
        bwd: &mut V,
    ) -> Option<(usize, usize)> {
        let (a, b) = (&a[r.a0..r.a1], &b[r.b0..r.b1]);
        let (n, m) = (a.len(), b.len());
        let delta = n as isize - m as isize;
        let dmax = ((n + m + 1) / 2 + 1) as isize;

        // Only `1` needs seeding: at every depth the recurrence reads diagonals
        // one *closer* to the centre than the ones it writes, never further, so
        // nothing stale is ever read and neither array needs clearing.
        fwd.set(1, 0);
        bwd.set(1, 0);

        for d in 0..=dmax {
            let mut k = d;
            while k >= -d {
                if !self.spend(1) {
                    return None;
                }
                let mut x = if k == -d || (k != d && fwd.get(k - 1) < fwd.get(k + 1)) {
                    fwd.get(k + 1)
                } else {
                    fwd.get(k - 1) + 1
                };
                let mut y = (x as isize - k) as usize;
                while x < n && y < m && a[x] == b[y] {
                    x += 1;
                    y += 1;
                }
                fwd.set(k, x);
                // The backward frontier is one depth behind, so only the
                // diagonals it has actually reached may be consulted. An odd
                // delta is when the two frontiers can meet on a forward step.
                if delta % 2 != 0 && d >= 1 && (delta - k).abs() <= d - 1 {
                    if x + bwd.get(delta - k) >= n {
                        return inside(r, x, y, n, m);
                    }
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
                let mut x = if k == -d || (k != d && bwd.get(k - 1) < bwd.get(k + 1)) {
                    bwd.get(k + 1)
                } else {
                    bwd.get(k - 1) + 1
                };
                let mut y = (x as isize - k) as usize;
                while x < n && y < m && a[n - x - 1] == b[m - y - 1] {
                    x += 1;
                    y += 1;
                }
                bwd.set(k, x);
                if delta % 2 == 0 && (delta - k).abs() <= d {
                    if fwd.get(delta - k) + x >= n {
                        return inside(r, n - x, m - y, n, m);
                    }
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
                    stack.push(Region { a0: a_at + len, a1: r.a1, b0: b_at + len, b1: r.b1 });
                    stack.push(Region { a0: r.a0, a1: a_at, b0: r.b0, b1: b_at });
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
    At { a_at: usize, b_at: usize, len: usize },
}

/// A split point, unless it is a corner of the region it came from.
fn inside(r: Region, x: usize, y: usize, n: usize, m: usize) -> Option<(usize, usize)> {
    if (x == 0 && y == 0) || (x == n && y == m) {
        return None;
    }
    Some((r.a0 + x, r.b0 + y))
}

/// Furthest-reaching path per diagonal, indexed by `x - y`, which runs negative.
struct V {
    offset: isize,
    buf: Vec<usize>,
}

impl V {
    fn new(span: usize) -> Self {
        // Diagonals reach |k| = dmax, and the recurrence reads k+1 at the edge.
        let offset = ((span + 1) / 2 + 3) as isize;
        Self { offset, buf: vec![0; 2 * offset as usize + 1] }
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
pub fn hunks(old: &[&str], new: &[&str], edits: &[Edit], context: usize) -> Vec<Hunk> {
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
        let lead = context.min(first.old_start as usize).min(first.new_start as usize);
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
                lines.push(line(LineKind::Context, Some(o), Some(n), old[o]));
                o += 1;
                n += 1;
            }
            for k in e.old() {
                lines.push(line(LineKind::Removed, Some(k), None, old[k]));
            }
            for k in e.new() {
                lines.push(line(LineKind::Added, None, Some(k), new[k]));
            }
            o = e.old_end as usize;
            n = e.new_end as usize;
        }
        while o < o_end {
            lines.push(line(LineKind::Context, Some(o), Some(n), old[o]));
            o += 1;
            n += 1;
        }

        out.push(Hunk {
            header: header(o_start, o_end - o_start, n_start, n_end - n_start, old, o_start),
            lines,
        });
        i = j + 1;
    }
    out
}

fn line(kind: LineKind, old_no: Option<usize>, new_no: Option<usize>, text: &str) -> DiffLine {
    DiffLine {
        kind,
        // Both sides are 0-based here and 1-based on screen.
        old_no: old_no.map(|n| n as u32 + 1),
        new_no: new_no.map(|n| n as u32 + 1),
        text: text.to_string(),
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
    old: &[&str],
    from: usize,
) -> String {
    let range = |start: usize, count: usize| match count {
        // Nothing on this side: git names the line the change sits *after*, and
        // spells the zero out. `@@ -0,0 +1,5 @@` is a new file.
        0 => format!("{start},0"),
        1 => format!("{}", start + 1),
        _ => format!("{},{count}", start + 1),
    };
    let mut h = format!("@@ -{} +{} @@", range(o_start, o_count), range(n_start, n_count));
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
fn enclosing<'a>(old: &[&'a str], from: usize) -> Option<&'a str> {
    old[from.saturating_sub(FUNCNAME_LOOKBACK)..from].iter().rev().find_map(|l| {
        let c = *l.as_bytes().first()?;
        (c.is_ascii_alphabetic() || c == b'_' || c == b'$').then(|| l.trim_end())
    })
}

// ---------------------------------------------------------------- the registry

/// Which algorithm each path gets, and how much context its hunks carry.
///
/// The same shape as [`Highlighters`](crate::syntax::Highlighters), for the same
/// reason: routing by path is what lets a specialist take `.json` without the
/// generalist knowing it exists. Selection is by *name* rather than by value
/// because a config file has to be able to express it — see
/// `docs/decisions/0012-config-is-data-behaviour-is-not.md`.
pub struct Differs {
    impls: Vec<Box<dyn Differ>>,
    routes: Vec<(Vec<String>, usize)>,
    fallback: usize,
    /// Unchanged lines shown around each change. git's default is 3.
    pub context: usize,
}

impl Default for Differs {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Differs {
    /// The three shipped algorithms, with Histogram selected.
    pub fn builtin() -> Self {
        let mut d = Self { impls: Vec::new(), routes: Vec::new(), fallback: 0, context: 3 };
        d.register(Histogram);
        d.register(Patience);
        d.register(Myers);
        d.select("histogram");
        d
    }

    /// Adds an implementation, replacing any already registered under the same
    /// name — so a built-in can be corrected rather than only added to, exactly
    /// as a language table can.
    pub fn register(&mut self, differ: impl Differ + 'static) {
        match self.impls.iter().position(|d| d.name() == differ.name()) {
            Some(i) => self.impls[i] = Box::new(differ),
            None => self.impls.push(Box::new(differ)),
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
                self.routes.push((keys.iter().map(|k| k.to_ascii_lowercase()).collect(), i));
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
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_ascii_lowercase();
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
    pub fn file(&self, path: &str, old: &[&str], new: &[&str]) -> FileDiff {
        self.file_using(None, path, old, new)
    }

    /// The same, with a runtime override of both the routes and the configured
    /// fallback.
    ///
    /// `Some(name)` is a frontend's live pick — the dropdown in the title bar.
    /// It overrides the *routes* too, deliberately: a user who asks for myers
    /// asked for the whole diff in myers, and quietly leaving `.json` on
    /// whatever it was routed to would make the control lie about what is on
    /// screen. A name that is not registered falls back to the configured
    /// behaviour rather than failing, because the caller is a click.
    pub fn file_using(
        &self,
        algorithm: Option<&str>,
        path: &str,
        old: &[&str],
        new: &[&str],
    ) -> FileDiff {
        let differ = algorithm
            .and_then(|name| self.by_name(name))
            .unwrap_or_else(|| self.for_path(path));
        let edits = differ.diff(path, old, new);
        FileDiff { path: path.to_string(), hunks: hunks(old, new, &edits, self.context) }
    }
}

// ----------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<&str> {
        if s.is_empty() {
            return Vec::new();
        }
        s.trim_end_matches('\n').split('\n').collect()
    }

    /// Every property the rest of the pipeline relies on, for any differ.
    fn verify(old: &[&str], new: &[&str], edits: &[Edit]) {
        let mut o = 0usize;
        let mut n = 0usize;
        let mut rebuilt: Vec<&str> = Vec::new();
        for (i, e) in edits.iter().enumerate() {
            assert!(!e.is_empty(), "edit {i} is empty: {e:?}");
            assert!(e.old_start as usize >= o, "edit {i} out of order: {e:?}");
            assert!(e.new_start as usize >= n, "edit {i} out of order: {e:?}");
            assert!(e.old_start <= e.old_end && e.new_start <= e.new_end, "{e:?}");
            assert!(e.old_end as usize <= old.len() && e.new_end as usize <= new.len(), "{e:?}");
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
            rebuilt.extend_from_slice(&new[e.new()]);
            o = e.old_end as usize;
            n = e.new_end as usize;
        }
        rebuilt.extend_from_slice(&old[o..]);
        assert_eq!(rebuilt, new, "applying the script did not produce the new file");
    }

    /// Length of the longest common subsequence, by the textbook table. The
    /// reference a minimal script is checked against.
    fn lcs(a: &[&str], b: &[&str]) -> usize {
        let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                t[i][j] =
                    if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
            }
        }
        t[0][0]
    }

    fn changed(edits: &[Edit]) -> usize {
        edits.iter().map(|e| e.old().len() + e.new().len()).sum()
    }

    const ALL: [&dyn Differ; 3] = [&Histogram, &Patience, &Myers];

    #[test]
    fn every_differ_produces_a_script_that_applies() {
        let old = lines("fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n");
        let new = lines("fn main() {\n    let x = 2;\n    let y = 3;\n    println!(\"{x}\");\n}\n");
        for d in ALL {
            let edits = d.diff("a.rs", &old, &new);
            verify(&old, &new, &edits);
            assert!(!edits.is_empty(), "{} found no change", d.name());
        }
    }

    #[test]
    fn identical_files_produce_no_edits() {
        let f = lines("a\nb\nc\n");
        for d in ALL {
            assert!(d.diff("x", &f, &f).is_empty(), "{}", d.name());
        }
    }

    #[test]
    fn one_side_empty_is_the_whole_file() {
        let f = lines("a\nb\nc\n");
        for d in ALL {
            let add = d.diff("x", &[], &f);
            verify(&[], &f, &add);
            assert_eq!(add, vec![Edit { old_start: 0, old_end: 0, new_start: 0, new_end: 3 }]);
            let del = d.diff("x", &f, &[]);
            verify(&f, &[], &del);
            assert_eq!(del, vec![Edit { old_start: 0, old_end: 3, new_start: 0, new_end: 0 }]);
            // And two empty files are not a change.
            assert!(d.diff("x", &[], &[]).is_empty());
        }
    }

    #[test]
    fn myers_is_minimal() {
        // The definition of the algorithm, checked against the textbook table.
        let old = lines("a\nb\nc\na\nb\nb\na\n");
        let new = lines("c\nb\na\nb\na\nc\n");
        let edits = Myers.diff("x", &old, &new);
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
            let old: Vec<&str> =
                (0..n).map(|_| alphabet[rand(letters as u64) as usize]).collect();
            let new: Vec<&str> =
                (0..m).map(|_| alphabet[rand(letters as u64) as usize]).collect();
            let edits = Myers.diff("x", &old, &new);
            verify(&old, &new, &edits);
            let ideal = old.len() + new.len() - 2 * lcs(&old, &new);
            assert_eq!(changed(&edits), ideal, "case {case}: {old:?} -> {new:?} gave {edits:?}");
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
            let old: Vec<&str> = (0..n).map(|_| alphabet[rand(letters as u64) as usize]).collect();
            let new: Vec<&str> = (0..m).map(|_| alphabet[rand(letters as u64) as usize]).collect();
            for d in ALL {
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
        let h = Histogram.diff("x.rs", &old, &new);
        verify(&old, &new, &h);
        assert_eq!(h.len(), 2, "expected a move, got {h:?}");
        assert!(
            h.iter().all(|e| e.old().is_empty() || e.new().is_empty()),
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
        let p = Patience.diff("x", &old, &new);
        verify(&old, &new, &p);
        assert_eq!(p, Myers.diff("x", &old, &new), "the fallback is not myers");
        let h = Histogram.diff("x", &old, &new);
        verify(&old, &new, &h);
    }

    #[test]
    fn an_exhausted_budget_degrades_to_a_replace_instead_of_stalling() {
        // What a fully rewritten generated file hits. Driven through `Ctx`
        // directly with a small budget rather than by feeding it 60k lines,
        // because the cost of reaching `MAX_STEPS` honestly *is* MAX_STEPS and
        // `cargo test -p plait-core` is meant to stay under a second.
        let old: Vec<String> = (0..400).map(|i| format!("old {i}")).collect();
        let new: Vec<String> = (0..400).map(|i| format!("new {i}")).collect();
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        let (a, b) = intern(&o, &n);

        let mut ctx = Ctx::new();
        ctx.steps = 50;
        let mut out = Vec::new();
        ctx.myers(&a, &b, Region::whole(&a, &b), &mut out);
        verify(&o, &n, &out);
        assert_eq!(
            out,
            vec![Edit { old_start: 0, old_end: 400, new_start: 0, new_end: 400 }],
            "an exhausted myers must say the region was replaced"
        );

        // The anchored ones charge their index build to the same budget, and a
        // region they cannot afford to index goes to myers, which cannot afford
        // it either — so the same replace comes out.
        let mut ctx = Ctx::new();
        ctx.steps = 50;
        let mut out = Vec::new();
        ctx.anchored(&a, &b, MAX_ANCHOR_OCCURRENCES, &mut out);
        verify(&o, &n, &out);
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn an_anchor_that_peels_one_line_at_a_time_still_finishes() {
        // The shape that makes recursion depth equal to file length: a unique
        // line between every pair of identical ones, so every anchor is one line
        // long and the region to the right of it is almost the whole file. As a
        // call stack that is a crash; as a work stack it is a loop.
        let old: Vec<String> =
            (0..2000).flat_map(|i| [format!("anchor {i}"), "x".to_string()]).collect();
        let new: Vec<String> =
            (0..2000).flat_map(|i| [format!("anchor {i}"), "y".to_string()]).collect();
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        for d in ALL {
            let edits = d.diff("generated.rs", &o, &n);
            verify(&o, &n, &edits);
        }
    }

    #[test]
    fn a_region_with_nothing_in_common_is_one_replace() {
        let old = lines("aaa\nbbb\n");
        let new = lines("ccc\nddd\n");
        for d in ALL {
            let edits = d.diff("x", &old, &new);
            verify(&old, &new, &edits);
            assert_eq!(
                edits,
                vec![Edit { old_start: 0, old_end: 2, new_start: 0, new_end: 2 }],
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
        let old: Vec<String> = (0..40_000).map(|i| format!("line {}", i % 3)).collect();
        let new: Vec<String> = (0..40_000).map(|i| format!("line {}", i % 4)).collect();
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        for d in ALL {
            let edits = d.diff("x", &o, &n);
            verify(&o, &n, &edits);
        }
    }

    #[test]
    fn a_fully_rewritten_generated_file_degrades_instead_of_stalling() {
        // 60k lines against 60k different lines is 10^10 Myers steps. The budget
        // has to turn that into a replace, and the result still has to apply.
        let old: Vec<String> = (0..60_000).map(|i| format!("old {i}")).collect();
        let new: Vec<String> = (0..60_000).map(|i| format!("new {i}")).collect();
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        let t = std::time::Instant::now();
        let edits = Myers.diff("bundle.js", &o, &n);
        verify(&o, &n, &edits);
        assert!(t.elapsed().as_secs() < 20, "took {:?}", t.elapsed());
    }

    // ------------------------------------------------------------ hunk output

    #[test]
    fn hunks_carry_context_and_both_line_numbers() {
        let old = lines("1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
        let new = lines("1\n2\n3\n4\nFIVE\n6\n7\n8\n9\n10\n");
        let edits = Histogram.diff("x", &old, &new);
        let hs = hunks(&old, &new, &edits, 3);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].header, "@@ -2,7 +2,7 @@");
        let kinds: Vec<LineKind> = hs[0].lines.iter().map(|l| l.kind).collect();
        use LineKind::*;
        assert_eq!(kinds, vec![Context, Context, Context, Removed, Added, Context, Context, Context]);
        // Numbers are 1-based and both sides advance over context.
        assert_eq!((hs[0].lines[0].old_no, hs[0].lines[0].new_no), (Some(2), Some(2)));
        let removed = hs[0].lines.iter().find(|l| l.kind == Removed).unwrap();
        assert_eq!((removed.old_no, removed.new_no), (Some(5), None));
        let added = hs[0].lines.iter().find(|l| l.kind == Added).unwrap();
        assert_eq!((added.old_no, added.new_no), (None, Some(5)));
    }

    #[test]
    fn nearby_changes_share_a_hunk_and_distant_ones_do_not() {
        let old: Vec<String> = (1..=40).map(|i| i.to_string()).collect();
        let mut new = old.clone();
        new[5] = "six".into();
        new[10] = "eleven".into(); // 5 lines away: context overlaps
        new[30] = "thirtyone".into(); // far away
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        let hs = hunks(&o, &n, &Histogram.diff("x", &o, &n), 3);
        assert_eq!(hs.len(), 2, "{:?}", hs.iter().map(|h| &h.header).collect::<Vec<_>>());
        // No line is printed twice: the merged hunk covers 3..14, the other 28..34.
        assert!(hs[0].lines.iter().filter(|l| l.kind == LineKind::Removed).count() == 2);
    }

    #[test]
    fn context_of_zero_prints_only_the_change() {
        let old = lines("a\nb\nc\n");
        let new = lines("a\nB\nc\n");
        let hs = hunks(&old, &new, &Histogram.diff("x", &old, &new), 0);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].lines.len(), 2);
        assert_eq!(hs[0].header, "@@ -2 +2 @@ a", "a count of one is written without it");
    }

    #[test]
    fn a_pure_insertion_names_the_line_before_it() {
        // git's convention, and it looks like an off-by-one until you know it:
        // an empty old side prints the line the insertion comes *after*.
        let old = lines("a\n");
        let new = lines("a\nb\n");
        let hs = hunks(&old, &new, &Myers.diff("x", &old, &new), 0);
        assert_eq!(hs[0].header, "@@ -1,0 +2 @@ a");

        let hs = hunks(&new, &old, &Myers.diff("x", &new, &old), 0);
        assert_eq!(hs[0].header, "@@ -2 +1,0 @@ a");
    }

    #[test]
    fn the_header_names_the_enclosing_declaration() {
        let old = lines("fn dispatch() {\n    a();\n    b();\n    c();\n}\n");
        let new = lines("fn dispatch() {\n    a();\n    B();\n    c();\n}\n");
        let hs = hunks(&old, &new, &Histogram.diff("x.rs", &old, &new), 1);
        assert_eq!(hs[0].header, "@@ -2,3 +2,3 @@ fn dispatch() {");
    }

    #[test]
    fn the_header_search_gives_up_rather_than_scanning_a_whole_file() {
        // A formatted blob: every line indented, so there is no declaration to
        // find and the search would otherwise walk to line 0 for every hunk.
        let mut old: Vec<String> = (0..2000).map(|i| format!("    \"k{i}\": {i},")).collect();
        old[0] = "root = {".into();
        let mut new = old.clone();
        new[1500] = "    \"k1500\": 99,".into();
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        let hs = hunks(&o, &n, &Histogram.diff("x.json", &o, &n), 3);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].header, "@@ -1498,7 +1498,7 @@", "no declaration within reach");

        // Within reach, it is still found.
        let hs = hunks(&o[1200..], &n[1200..], &Myers.diff("x", &o[1200..], &n[1200..]), 0);
        assert!(hs[0].header.ends_with("@@"), "{}", hs[0].header);
    }

    #[test]
    fn a_hunk_at_the_very_start_or_end_does_not_run_off_the_file() {
        let old = lines("a\nb\n");
        let new = lines("A\nb\n");
        let hs = hunks(&old, &new, &Myers.diff("x", &old, &new), 10);
        assert_eq!(hs[0].header, "@@ -1,2 +1,2 @@");
        assert_eq!(hs[0].lines.len(), 3);

        let old = lines("a\nb\n");
        let new = lines("a\nB\n");
        let hs = hunks(&old, &new, &Myers.diff("x", &old, &new), 10);
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
        let old: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        let new: Vec<String> = (0..500)
            .filter(|i| i % 7 != 0)
            .map(|i| if i % 11 == 0 { format!("changed {i}") } else { format!("line {i}") })
            .collect();
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        for d in ALL {
            let edits = d.diff("x", &o, &n);
            verify(&o, &n, &edits);
            for h in hunks(&o, &n, &edits, 3) {
                for l in &h.lines {
                    if let Some(no) = l.old_no {
                        assert_eq!(o[no as usize - 1], l.text, "{} old line {no}", d.name());
                    }
                    if let Some(no) = l.new_no {
                        assert_eq!(n[no as usize - 1], l.text, "{} new line {no}", d.name());
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
        fn diff(&self, _path: &str, old: &[&str], new: &[&str]) -> Vec<Edit> {
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
        assert_eq!(d.for_path("a.go").name(), "reverse", "unrouted paths keep the fallback");
        assert!(!d.route(&["go"], "nope"));
    }

    #[test]
    fn registering_a_name_twice_replaces_it() {
        struct FakeMyers;
        impl Differ for FakeMyers {
            fn name(&self) -> &'static str {
                "myers"
            }
            fn diff(&self, _: &str, _: &[&str], _: &[&str]) -> Vec<Edit> {
                Vec::new()
            }
        }
        let mut d = Differs::builtin();
        d.register(FakeMyers);
        assert_eq!(d.names().len(), 3, "a replacement must not also append");
        assert!(d.select("myers"));
        let f = lines("a\nb\n");
        assert!(d.for_path("x").diff("x", &f, &[]).is_empty(), "the built-in still ran");
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
        let overridden = d.file_using(Some("myers"), "x.rs", &old, &new);
        assert_ne!(
            routed.hunks[0].lines.len(),
            overridden.hunks[0].lines.len(),
            "the override did not beat the route"
        );

        // An unregistered name is a click that cannot be honoured, so it falls
        // back rather than producing nothing.
        assert_eq!(d.file_using(Some("nope"), "x.rs", &old, &new), routed);
        assert_eq!(d.file_using(None, "x.rs", &old, &new), routed);
    }

    #[test]
    fn an_implementation_is_reachable_by_name() {
        let mut d = Differs::builtin();
        d.register(Reverse);
        assert_eq!(d.by_name("reverse").map(|x| x.name()), Some("reverse"));
        assert_eq!(d.by_name("histogram").map(|x| x.name()), Some("histogram"));
        assert!(d.by_name("nope").is_none());
    }

    #[test]
    fn the_context_setting_reaches_the_hunks() {
        let old: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let mut new = old.clone();
        new[10] = "ten!".into();
        let o: Vec<&str> = old.iter().map(String::as_str).collect();
        let n: Vec<&str> = new.iter().map(String::as_str).collect();
        for context in [0, 1, 3, 7] {
            let mut d = Differs::builtin();
            d.context = context;
            let f = d.file("x", &o, &n);
            assert_eq!(f.hunks[0].lines.len(), 2 + 2 * context, "context {context}");
        }
    }
}
