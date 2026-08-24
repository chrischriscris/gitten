//! Everything here is pure: no GPUI, no gitoxide, no I/O.
//!
//! That is the whole point. If the shell has to be rewritten — GPUI to Electron,
//! or the other way — nothing in this crate changes. Keep it that way: the day
//! something in here needs to know what a window is, the boundary is gone.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

pub mod align;
pub mod command;
pub mod differ;
pub mod font;
pub mod graph;
pub mod host;
pub mod markdown;
pub mod prepared;
pub mod refs;
pub mod rows;
pub mod runs;
pub mod search;
pub mod select;
pub mod status;
pub mod syntax;
pub mod theme;
pub mod view;
pub mod wrap;

// ---------------------------------------------------------------- commit log

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    /// Built once and never mutated: a boxed slice carries no growth slack,
    /// which across 82k commits of git/git is real memory for nothing.
    pub parents: Box<[String]>,
    /// Interned by text in `parse_log` — a few thousand authors repeat across
    /// tens of thousands of commits, and one `Arc<str>` per distinct name
    /// beats one `String` per commit.
    pub author: Arc<str>,
    pub timestamp: i64,
    pub subject: String,
}

/// The hash behind every intern map in this crate: rustc's own Fx construction,
/// a rotate-xor-multiply over eight-byte chunks.
///
/// `HashMap`'s default SipHash cost more per lookup than the rest of the
/// interning put together on short keys — measured at about a third of
/// `parse_log`'s regression before this replaced it, and at **half of the
/// differ's whole runtime** when it reached the line-intern map too (see
/// `docs/measurements.md`). Not cryptographic, and it does not need to be: a
/// collision only costs a probe, because the map compares the real bytes either
/// way. Nothing here hashes anything an attacker chooses *and* keeps, which is
/// the only reason SipHash's flooding resistance would be worth paying for.
///
/// The 0xff terminator keeps `"ab" + "c"` and `"abc"` apart in one stream, as
/// `str`'s default hash does.
#[derive(Default)]
pub(crate) struct FxHasher {
    hash: u64,
}

/// A `HashMap` on [`FxHasher`]. A named alias rather than the bound spelled out
/// at each use, because the point is that every intern map in the crate is the
/// *same* map — the line map having quietly stayed on SipHash while the author
/// map moved is exactly what this prevents.
pub(crate) type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.hash = (self.hash.rotate_left(5) ^ u64::from_le_bytes(buf)).wrapping_mul(FX_SEED);
        }
    }
    fn write_u8(&mut self, b: u8) {
        self.hash = (self.hash.rotate_left(5) ^ u64::from(b)).wrapping_mul(FX_SEED);
    }
    // `str`'s default hash walks `write` then `write_u8(0xff)` — the same
    // stream this type would produce, so no override needed.
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// Parses the output of `fixtures/dump.sh` (see that script for the format).
///
/// Fields are \x1f-separated, records \x1e-separated — control characters git
/// will never emit inside a subject, so there is nothing to escape.
pub fn parse_log(raw: &str) -> Vec<Commit> {
    // Counting separators first costs one byte scan and buys a right-sized
    // vector: at 100k commits the growth path was re-copying the whole list
    // several times over.
    let mut out = Vec::with_capacity(raw.bytes().filter(|b| *b == b'\x1e').count() + 1);
    // Interned on the borrowed field: a repeat costs one hash lookup and no
    // allocation, and the keys borrow `raw`, which lives past the loop. Hashed
    // with [`FxHasher`] — see there for why the default hasher lost money —
    // and sized for a real history up front: growing this table mid-parse
    // rehashed everything parsed so far, twice, on the way to a few thousand
    // authors.
    let mut authors: FxHashMap<&str, Arc<str>> =
        FxHashMap::with_capacity_and_hasher(4096, BuildHasherDefault::default());
    for rec in raw.split('\u{1e}') {
        let rec = rec.trim();
        if rec.is_empty() {
            continue;
        }
        // Fields are taken by position without collecting them first; a record
        // with fewer than six has nothing to build and is skipped, exactly as
        // when they were gathered into a Vec to be counted. A subject that
        // itself contains \x1f still reads as field five — everything after it
        // is simply never asked for.
        let mut fields = rec.split('\u{1f}');
        let (Some(sha), Some(short), Some(parents), Some(author), Some(ts), Some(subject)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        out.push(Commit {
            sha: sha.to_string(),
            short: short.to_string(),
            parents: parents.split_whitespace().map(str::to_string).collect(),
            author: match authors.get(author) {
                Some(seen) => Arc::clone(seen),
                None => {
                    let seen: Arc<str> = Arc::from(author);
                    authors.insert(author, Arc::clone(&seen));
                    seen
                }
            },
            timestamp: ts.parse().unwrap_or(0),
            subject: subject.to_string(),
        });
    }
    out
}

// ----------------------------------------------------------------- the graph

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    /// Which lane this commit's dot sits in.
    pub lane: usize,
    /// Lanes with a line passing straight through this row.
    pub through: Vec<usize>,
    /// Lanes that converge into this commit (branch point, drawn as a curve in).
    pub merges: Vec<usize>,
    /// Lanes forked off for a merge commit's 2nd+ parents (drawn as a curve out).
    pub forks: Vec<usize>,
}

/// Assigns a lane to every commit in `commits`, which must be in `git log`
/// order (newest first). This is the algorithmic heart of the graph screen and
/// it is deliberately here rather than in the renderer — it is testable without
/// opening a window.
pub fn assign_lanes(commits: &[Commit]) -> Vec<GraphRow> {
    // lanes[i] = the sha lane `i` is currently waiting to draw, borrowed out
    // of `commits` rather than cloned into it: claiming a lane or re-pointing
    // one then moves two words instead of allocating, and the scan itself runs
    // over a handful of cache-hot entries either way.
    let mut lanes: Vec<Option<&str>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for c in commits {
        let sha = c.sha.as_str();
        let lane = match lanes.iter().position(|l| *l == Some(sha)) {
            Some(i) => i,
            None => claim_lane(&mut lanes, sha),
        };

        // Any *other* lane waiting on this sha converges here.
        let mut merges = Vec::new();
        for (i, l) in lanes.iter_mut().enumerate() {
            if i != lane && *l == Some(sha) {
                merges.push(i);
                *l = None;
            }
        }

        let through: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(i, l)| *i != lane && l.is_some())
            .map(|(i, _)| i)
            .collect();

        // Re-point our lane at the first parent; fork new lanes for the rest.
        let mut parents = c.parents.iter();
        lanes[lane] = parents.next().map(|p| p.as_str());
        let forks: Vec<usize> = parents
            .map(|p| claim_lane(&mut lanes, p.as_str()))
            .collect();

        rows.push(GraphRow {
            lane,
            through,
            merges,
            forks,
        });
    }
    rows
}

fn claim_lane<'a>(lanes: &mut Vec<Option<&'a str>>, sha: &'a str) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => {
            lanes[i] = Some(sha);
            i
        }
        None => {
            lanes.push(Some(sha));
            lanes.len() - 1
        }
    }
}

/// Two letters for an author, the way a dense list wants them: first and last
/// name, or the first two characters of a single-word name. Uppercased, so a
/// column of them lines up as initials rather than as words.
///
/// Here rather than in the shell because it is a pure text transform, and the
/// terminal frontend wants exactly the same two letters.
pub fn initials(author: &str) -> String {
    let mut words = author.split_whitespace();
    let first = words.next().unwrap_or_default();
    let mut out = String::with_capacity(2);
    let mut push = |c: Option<char>| out.extend(c.into_iter().flat_map(char::to_uppercase));
    match words.last() {
        // "Junio C Hamano" -> JH: the last name, never the middle one.
        Some(last) => {
            push(first.chars().next());
            push(last.chars().next());
        }
        // One word is all there is: "torvalds" -> TO, "x" -> X, "" -> "".
        None => {
            let mut cs = first.chars();
            push(cs.next());
            push(cs.next());
        }
    }
    out
}

// ------------------------------------------------------------------- the diff

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// Shared with [`prepared::Line`](crate::prepared::Line) and the rows every
    /// frontend builds from it: one allocation per distinct line of text for
    /// the whole pipeline instead of one per copy of it. `Arc<str>` and not
    /// `String` so `clip`'s fast path and a layout change's rebuild are
    /// refcount bumps.
    pub text: Arc<str>,
    /// This line is part of a block that moved rather than changed — see
    /// [`differ::moves`](crate::differ::moves).
    ///
    /// A flag beside `kind` and not a fourth `LineKind`, deliberately. A moved
    /// line is still an addition or a removal, and everything that reasons about
    /// runs of them — [`align`](crate::align::align), `replace_pairs`, the adds
    /// and dels counts — must keep working unchanged. Only the drawing cares.
    pub moved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<Hunk>,
}

/// Whether a patch's own line terminators are CRLF, decided once for the whole
/// file rather than per line — because per line it is not decidable.
///
/// A content line of a CRLF *file* ends with `\r` inside the patch; so does every
/// line of a patch that was itself saved with CRLF terminators. The two are
/// indistinguishable in isolation, and guessing per line means a patch of a
/// Windows file loses the very bytes it is about. The whole file settles it: if
/// *every* line carries a `\r` then the `\r` is punctuation, and if any line does
/// not then the ones that do are content. This is the rule `git apply` uses, and
/// it is why the decision is taken before a single line is parsed.
fn crlf_terminated(raw: &str) -> bool {
    let mut any = false;
    for line in raw.split('\n') {
        // The empty tail after a trailing newline is not a line.
        if line.is_empty() {
            continue;
        }
        if !line.ends_with('\r') {
            return false;
        }
        any = true;
    }
    any
}

/// Parses `git diff` unified output. Enough for the spike; binary files,
/// renames and mode changes are skipped rather than modelled.
///
/// **A `\r` that is content stays in the line.** `str::lines()` was what this
/// used, and it strips a trailing `\r` unconditionally — so every line of a patch
/// of a CRLF file silently lost the byte the patch existed to show, and the
/// repository door (which keeps it, see `gitten_git`) and this one disagreed
/// about the text of the same commit. See [`crlf_terminated`] for how the
/// ambiguity is settled.
pub fn parse_unified_diff(raw: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let strip_cr = crlf_terminated(raw);

    for line in raw.split('\n') {
        let line = match strip_cr {
            true => line.strip_suffix('\r').unwrap_or(line),
            false => line,
        };
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/<old> b/<new>`; a path may contain spaces, so split on the
            // ` b/` boundary rather than on whitespace. The new path is what we
            // render. (TODO: dequote git's `"…"` path form for unusual bytes.)
            let path = rest
                .rfind(" b/")
                .map(|i| rest[i + 3..].to_string())
                .unwrap_or_else(|| "?".to_string());
            files.push(FileDiff {
                path,
                hunks: Vec::new(),
            });
            continue;
        }
        if line.starts_with("@@ ") {
            let (o, n) = parse_hunk_header(line);
            old_no = o;
            new_no = n;
            if let Some(f) = files.last_mut() {
                f.hunks.push(Hunk {
                    header: line.to_string(),
                    lines: Vec::new(),
                });
            }
            continue;
        }
        let Some(hunk) = files.last_mut().and_then(|f| f.hunks.last_mut()) else {
            // Before any hunk of the current file: the `+++`/`---`/`index`
            // metadata lines live here, and there is nothing to attach a
            // content line to anyway, so the guard skips them. Once inside a
            // hunk, a `-`/`+` line is genuine content — `-- a comment` renders
            // as a removed line, not as a header to drop.
            continue;
        };
        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b' ') => (LineKind::Context, &line[1..]),
            _ => continue, // "\ No newline at end of file", blank trailing lines
        };
        let (o, n) = match kind {
            LineKind::Added => (None, Some(new_no)),
            LineKind::Removed => (Some(old_no), None),
            LineKind::Context => (Some(old_no), Some(new_no)),
        };
        if kind != LineKind::Added {
            old_no += 1;
        }
        if kind != LineKind::Removed {
            new_no += 1;
        }
        // Never moved: a `.diff` file has already been rendered by whoever
        // produced it, and its move detection — if it had any — was colour.
        hunk.lines.push(DiffLine {
            kind,
            old_no: o,
            new_no: n,
            text: Arc::from(text),
            moved: false,
        });
    }
    files
}

/// A hunk header split into the coordinates and the code around them:
/// `("@@ -41,9 +41,11 @@", "fn dispatch() {")`.
///
/// Here rather than in a renderer because every client draws this string and the
/// split is the same everywhere: the numbers are furniture — the same kind of
/// thing a line number is, and drawn in the same colour — while the enclosing
/// declaration git appends is the half a reader actually wants. Drawn as one run
/// they read as equally important, which they are not.
///
/// The whole header goes in the first half when there is no second `@@`, so a
/// caller never has to check.
pub fn hunk_parts(header: &str) -> (&str, &str) {
    // The second `@@`, not the first: a filename in the tail cannot be confused
    // for the marker, because the marker is always the closing one of a pair.
    let Some(end) = header
        .find("@@")
        .and_then(|a| header[a + 2..].find("@@").map(|b| a + 2 + b + 2))
    else {
        return (header, "");
    };
    (&header[..end], header[end..].trim_start())
}

/// `@@ -41,9 +41,11 @@ ...` -> (41, 41)
///
/// Only the coordinate section is scanned — [`hunk_parts`] splits off the
/// function-context tail first, so a `-1;` or a `- item` bullet in the code git
/// appends cannot overwrite the real line numbers.
fn parse_hunk_header(line: &str) -> (u32, u32) {
    let (coords, _tail) = hunk_parts(line);
    let mut old = 0;
    let mut new = 0;
    for tok in coords.split_whitespace() {
        let num = |s: &str| s.split(',').next().unwrap_or("0").parse().unwrap_or(0);
        if let Some(s) = tok.strip_prefix('-') {
            old = num(s);
        } else if let Some(s) = tok.strip_prefix('+') {
            new = num(s);
        }
    }
    (old, new)
}

// ------------------------------------------------------------ intraline diff

/// A byte range within a line.
///
/// Offsets are `u32`: they index *clipped* text, whose length the frontend
/// budgets by window width — always far below `u32::MAX`. 8 bytes a span keeps
/// a 700k-row diff's spans in cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// Splits a line into word-ish tokens: runs of alphanumerics/underscore, and
/// every other character on its own. Word granularity, not character — a
/// char-level diff of code highlights every bracket and reads as confetti.
///
/// Tokens are `(offset, length)` pairs appended to a caller-owned buffer, so
/// both sides of a pair share one allocation and a token is 8 bytes rather
/// than the 24 an owned slice-and-offset tuple costs.
fn push_tokens(out: &mut Vec<(u32, u32)>, line: &str) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        if is_word(b[i]) {
            while i < b.len() && is_word(b[i]) {
                i += 1;
            }
        } else {
            // Keep multi-byte characters intact.
            i += 1;
            while i < b.len() && (b[i] & 0xC0) == 0x80 {
                i += 1;
            }
        }
        out.push((start as u32, (i - start) as u32));
    }
}

/// A token's bytes back again, for comparing one against another. Token
/// boundaries are char boundaries by construction in [`push_tokens`].
fn token_text(side: &str, t: (u32, u32)) -> &str {
    &side[t.0 as usize..(t.0 + t.1) as usize]
}

/// Above this many tokens on either side, skip word-level diffing entirely.
///
/// The LCS table is `a * b` cells. Real repositories contain minified bundles
/// and base64 blobs — a single line of ~14k tokens was measured in the wild,
/// which would allocate over a gigabyte for one line pair. The line still
/// renders with its add/remove background; it just gets no word highlighting,
/// which is the right trade when the line is machine-generated anyway.
pub const MAX_INTRALINE_TOKENS: usize = 1000;

/// Below this, a pair is not a rewrite of each other and gets no word-level
/// highlighting at all — the line still carries its add/remove background.
///
/// [`replace_pairs`] matches a run of removals to the additions after it by
/// position, which is right when someone edited N lines in place and wrong when
/// the counts happen to line up. Measured over the fixtures: of 9,447 pairs in
/// the zig->rust migration none sit below 0.60 similarity, and the lowest is a
/// genuine rewrite (`#define ZIG_DECL` -> `#define RUST_DECL`). In the
/// deletion-heavy fixture 15.6% fall below this floor, and by inspection every
/// one of them is junk — `/**` paired against `// Historical note: ...` at 0.0.
///
/// Highlighting those is worse than useless: it reports a rewrite that never
/// happened, and it drags the whole line under the changed-word background,
/// where dim text stops being legible.
pub const MIN_INTRALINE_SIMILARITY: f32 = 0.4;

/// Word-level diff of one removed/added line pair, returning the byte ranges
/// that actually changed on each side.
///
/// This is the *second* pass — it runs only on lines a line-level diff already
/// paired as a replace. That is why a plain LCS is the right call despite being
/// quadratic: the inputs are one line each, not one file. See
/// [`MAX_INTRALINE_TOKENS`] for the case where that assumption breaks.
pub fn intraline(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    // Offsets are u32, so a line beyond 4 GB has no representation. prepare
    // clips at 2000 characters; the guard keeps the assumption honest for a
    // direct caller that does not.
    if old.len() > u32::MAX as usize || new.len() > u32::MAX as usize {
        return (Vec::new(), Vec::new());
    }
    let mut tokens: Vec<(u32, u32)> = Vec::with_capacity(old.len() / 4 + new.len() / 4 + 8);
    push_tokens(&mut tokens, old);
    let na = tokens.len();
    push_tokens(&mut tokens, new);
    let (a, b) = tokens.split_at(na);

    if a.len() > MAX_INTRALINE_TOKENS || b.len() > MAX_INTRALINE_TOKENS {
        return (Vec::new(), Vec::new());
    }

    // Classic LCS table over tokens — one flat allocation of u32 rather than a
    // Vec per row of usize: an entry never exceeds either side's token count,
    // which the cap above bounds far below u32 range, and n+1 heap blocks per
    // line pair was most of what a small pair cost.
    let w = b.len() + 1;
    let mut lcs = match (a.len() + 1).checked_mul(w) {
        Some(cells) => vec![0u32; cells],
        None => return (Vec::new(), Vec::new()),
    };
    for i in (0..a.len()).rev() {
        // Row i is written from row i+1's final values and row i's own right
        // neighbour, so the split borrows them apart without aliasing.
        let (upper, lower) = lcs.split_at_mut((i + 1) * w);
        let cur = &mut upper[i * w..];
        let ta = token_text(old, a[i]);
        for j in (0..b.len()).rev() {
            cur[j] = if ta == token_text(new, b[j]) {
                lower[j + 1] + 1
            } else {
                lower[j].max(cur[j + 1])
            };
        }
    }

    // The table's corner is the length of the longest common subsequence, so
    // the similarity of the pair is already paid for.
    let common = lcs[0];
    let similarity = 2.0 * common as f32 / (a.len() + b.len()) as f32;
    if similarity < MIN_INTRALINE_SIMILARITY {
        return (Vec::new(), Vec::new());
    }

    let (mut old_spans, mut new_spans) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if token_text(old, a[i]) == token_text(new, b[j]) {
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * w + j] >= lcs[i * w + j + 1] {
            push_span(&mut old_spans, a[i].0, a[i].0 + a[i].1);
            i += 1;
        } else {
            push_span(&mut new_spans, b[j].0, b[j].0 + b[j].1);
            j += 1;
        }
    }
    while i < a.len() {
        push_span(&mut old_spans, a[i].0, a[i].0 + a[i].1);
        i += 1;
    }
    while j < b.len() {
        push_span(&mut new_spans, b[j].0, b[j].0 + b[j].1);
        j += 1;
    }
    coalesce(&mut old_spans, old);
    coalesce(&mut new_spans, new);
    (old_spans, new_spans)
}

/// Merge into the previous span when adjacent, so a changed phrase highlights
/// as one block rather than a row of separate token boxes.
fn push_span(spans: &mut Vec<Span>, start: u32, end: u32) {
    match spans.last_mut() {
        Some(last) if last.end == start => last.end = end,
        _ => spans.push(Span { start, end }),
    }
}

/// Close the gaps that contain only whitespace.
///
/// The LCS happily matches the space between two changed words, which leaves a
/// one-character hole in the highlight for every space in a rewritten sentence —
/// on screen a run of prose comes out as a row of separate green blocks with the
/// background showing through between them. A space between two changed words
/// belongs to the change.
fn coalesce(spans: &mut Vec<Span>, text: &str) {
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans.drain(..) {
        match merged.last_mut() {
            Some(last)
                if text
                    .get(last.end as usize..s.start as usize)
                    .is_some_and(|gap| !gap.is_empty() && gap.trim().is_empty()) =>
            {
                last.end = s.end;
            }
            _ => merged.push(s),
        }
    }
    *spans = merged;
}

/// Pairs each removed line in a hunk with the added line that replaced it.
///
/// A run of N removals immediately followed by M additions pairs index-wise;
/// unmatched leftovers are pure adds or deletes and get no highlighting.
///
/// One scan, shared with [`align`](crate::align::align), because a side-by-side
/// view has to put the same two lines on the same row that this hands to the
/// intraline pass — see that module for what happens when they disagree.
pub fn replace_pairs(hunk: &Hunk) -> Vec<(usize, usize)> {
    let kinds: Vec<LineKind> = hunk.lines.iter().map(|l| l.kind).collect();
    crate::align::pairs(&kinds)
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(sha: &str, parents: &[&str]) -> Commit {
        Commit {
            sha: sha.into(),
            short: sha.into(),
            parents: parents.iter().map(|s| s.to_string()).collect(),
            author: "t".into(),
            timestamp: 0,
            subject: "s".into(),
        }
    }

    /// The text of every line of a parsed patch, in order.
    fn texts(files: &[FileDiff]) -> Vec<String> {
        files
            .iter()
            .flat_map(|f| &f.hunks)
            .flat_map(|h| &h.lines)
            .map(|l| l.text.to_string())
            .collect()
    }

    #[test]
    fn a_carriage_return_that_is_content_survives_the_patch_parser() {
        // A patch of a file whose endings changed: the `-` lines are LF and the
        // `+` lines carry a CR *inside* the patch. `str::lines()` ate it, which
        // made this door disagree with the repository door about the text of the
        // same commit — and a patch of a CRLF file lose the byte it is about.
        let raw = "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n\
                   @@ -1,2 +1,2 @@\n-alpha\n-beta\n+alpha\r\n+beta\r\n";
        assert_eq!(
            texts(&parse_unified_diff(raw)),
            ["alpha", "beta", "alpha\r", "beta\r"]
        );
    }

    #[test]
    fn a_patch_saved_with_crlf_terminators_keeps_none_of_them() {
        // Every line ends with `\r`, so every `\r` is punctuation — including the
        // one on the path, which would otherwise put a control character in a
        // file name.
        let raw = "diff --git a/f.txt b/f.txt\r\n--- a/f.txt\r\n+++ b/f.txt\r\n\
                   @@ -1,1 +1,1 @@\r\n-alpha\r\n+beta\r\n";
        let files = parse_unified_diff(raw);
        assert_eq!(files[0].path, "f.txt", "a CR reached the path");
        assert_eq!(texts(&files), ["alpha", "beta"]);
    }

    #[test]
    fn a_crlf_patch_of_a_crlf_file_keeps_the_inner_carriage_return() {
        // Both at once: the terminator is stripped and the content's own `\r`
        // stays, because there were two.
        let raw = "diff --git a/f.txt b/f.txt\r\n@@ -1,1 +1,1 @@\r\n-alpha\r\r\n+beta\r\r\n";
        assert_eq!(texts(&parse_unified_diff(raw)), ["alpha\r", "beta\r"]);
    }

    #[test]
    fn deciding_the_terminator_needs_the_whole_patch() {
        assert!(crlf_terminated("a\r\nb\r\n"));
        assert!(crlf_terminated("a\r\nb\r"), "no final newline");
        assert!(!crlf_terminated("a\r\nb\n"), "one bare LF settles it");
        assert!(!crlf_terminated("a\nb\n"));
        assert!(!crlf_terminated(""), "nothing to decide about");
        assert!(!crlf_terminated("\n\n"));
    }

    #[test]
    fn a_hunk_header_splits_into_coordinates_and_code() {
        let (marker, code) = hunk_parts("@@ -41,9 +41,11 @@ fn dispatch() {");
        assert_eq!(marker, "@@ -41,9 +41,11 @@");
        assert_eq!(code, "fn dispatch() {");
        // No tail: every client draws the whole thing rather than checking.
        assert_eq!(hunk_parts("@@ -1 +1 @@"), ("@@ -1 +1 @@", ""));
        // A tail that itself contains the marker still splits at the pair.
        let (marker, code) = hunk_parts("@@ -1 +1 @@ fn f() { // @@ here");
        assert_eq!(marker, "@@ -1 +1 @@");
        assert_eq!(code, "fn f() { // @@ here");
        // Not a header at all: legible, not a panic.
        assert_eq!(hunk_parts(""), ("", ""));
        assert_eq!(hunk_parts("nonsense"), ("nonsense", ""));
    }

    #[test]
    fn log_round_trips() {
        let raw = "abc\u{1f}abc\u{1f}def ghi\u{1f}Ada\u{1f}1700000000\u{1f}Fix the thing\u{1e}\
                   def\u{1f}def\u{1f}\u{1f}Ada\u{1f}1699999999\u{1f}Initial commit\u{1e}";
        let c = parse_log(raw);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].parents.to_vec(), vec!["def", "ghi"]);
        assert_eq!(c[0].subject, "Fix the thing");
        assert!(c[1].parents.is_empty());
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        let cs = [commit("a", &["b"]), commit("b", &["c"]), commit("c", &[])];
        let rows = assign_lanes(&cs);
        assert!(rows.iter().all(|r| r.lane == 0));
        assert!(rows.iter().all(|r| r.through.is_empty()));
    }

    #[test]
    fn merge_forks_a_lane_and_branch_point_collapses_it() {
        //   a (merge of b, c)
        //   |\
        //   b c
        //   |/
        //   d
        let cs = [
            commit("a", &["b", "c"]),
            commit("b", &["d"]),
            commit("c", &["d"]),
            commit("d", &[]),
        ];
        let rows = assign_lanes(&cs);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[0].forks, vec![1], "second parent opens a new lane");
        assert_eq!(rows[1].lane, 0);
        assert_eq!(rows[1].through, vec![1], "c's lane passes through b's row");
        assert_eq!(rows[2].lane, 1);
        // Both lanes were waiting on d; reaching it collapses them back to one.
        assert_eq!(rows[3].lane, 0);
        assert_eq!(rows[3].merges, vec![1]);
    }

    #[test]
    fn initials_are_two_letters_however_the_name_is_shaped() {
        assert_eq!(initials("Junio C Hamano"), "JH");
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("torvalds"), "TO");
        assert_eq!(initials("x"), "X");
        assert_eq!(initials(""), "");
        assert_eq!(initials("  spaced   out  "), "SO");
        assert_eq!(initials("émile zola"), "ÉZ");
        assert_eq!(
            initials("émile"),
            "ÉM",
            "a multi-byte first letter is still one letter"
        );
    }

    #[test]
    fn diff_tracks_line_numbers_on_both_sides() {
        let raw = "\
diff --git a/host.go b/host.go
index 1111111..2222222 100644
--- a/host.go
+++ b/host.go
@@ -41,4 +41,5 @@ func Dispatch() {
 	for _, ext := range h.enabled {
-		go ext.Run(ev)
+		if err := h.pool.Submit(ev); err != nil {
+			return err
+		}
 	}
";
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "host.go");
        let lines = &files[0].hunks[0].lines;
        assert_eq!(lines[0].kind, LineKind::Context);
        assert_eq!((lines[0].old_no, lines[0].new_no), (Some(41), Some(41)));
        assert_eq!(lines[1].kind, LineKind::Removed);
        assert_eq!((lines[1].old_no, lines[1].new_no), (Some(42), None));
        assert_eq!(lines[2].kind, LineKind::Added);
        assert_eq!((lines[2].old_no, lines[2].new_no), (None, Some(42)));
        // context after the change: old side advanced 1, new side advanced 3
        let last = lines.last().unwrap();
        assert_eq!((last.old_no, last.new_no), (Some(43), Some(45)));
    }

    #[test]
    fn a_function_context_tail_is_not_read_as_coordinates() {
        // The `-1;` in the trailing declaration used to overwrite the old line
        // number with 0, so the whole hunk's gutter was wrong.
        let raw = "\
diff --git a/x.rs b/x.rs
@@ -10,4 +10,4 @@ const X: i32 = -1;
 keep
-old
+new
 tail
";
        let f = parse_unified_diff(raw);
        let lines = &f[0].hunks[0].lines;
        assert_eq!((lines[0].old_no, lines[0].new_no), (Some(10), Some(10)));
    }

    #[test]
    fn a_leading_dash_bullet_in_the_tail_is_not_read_as_coordinates() {
        // A markdown or list-style tail whose first token is `- item` must not
        // reset the old coordinate the way the `-1;` case did.
        let raw = "\
diff --git a/x.md b/x.md
@@ -5,2 +5,2 @@ - item
 keep
-old
+new
";
        let f = parse_unified_diff(raw);
        let lines = &f[0].hunks[0].lines;
        assert_eq!((lines[0].old_no, lines[0].new_no), (Some(5), Some(5)));
    }

    #[test]
    fn a_removed_comment_beginning_with_two_dashes_is_content_not_metadata() {
        // `-- a comment` (SQL/Lua/Haskell/Ada) appears as `--- a comment` in the
        // patch. The old metadata skip dropped it *and* failed to advance the
        // counter, so every following line was numbered one too low.
        let raw = "\
diff --git a/q.sql b/q.sql
@@ -1,3 +1,3 @@
 keep
--- a comment
+++ b comment
 tail
";
        let f = parse_unified_diff(raw);
        let lines = &f[0].hunks[0].lines;
        assert_eq!(lines[1].kind, LineKind::Removed);
        assert_eq!(&*lines[1].text, "-- a comment", "one dash stripped");
        assert_eq!((lines[1].old_no, lines[1].new_no), (Some(2), None));
        assert_eq!(lines[2].kind, LineKind::Added);
        assert_eq!(&*lines[2].text, "++ b comment");
        // The trailing context is numbered correctly, not off by one.
        let last = lines.last().unwrap();
        assert_eq!((last.old_no, last.new_no), (Some(3), Some(3)));
    }

    #[test]
    fn a_path_containing_spaces_survives_the_diff_git_header() {
        // Splitting on whitespace yielded "?" and routed the file to the
        // fallback differ; the ` b/` boundary is what git actually delimits on.
        let raw = "\
diff --git a/dir with spaces/a.rs b/dir with spaces/a.rs
@@ -1,1 +1,1 @@
-old
+new
";
        let f = parse_unified_diff(raw);
        assert_eq!(f[0].path, "dir with spaces/a.rs");
    }

    #[test]
    fn intraline_marks_only_the_changed_tokens() {
        let old = "if !ext.Handles(ev.Kind) {";
        let new = "if !ext.Handles(ev.Kind) || h.budget.Exhausted() {";
        let (o, n) = intraline(old, new);
        assert!(o.is_empty(), "nothing was removed, got {:?}", o);
        assert_eq!(n.len(), 1, "the inserted clause should be one merged span");
        // Which of the two equivalent spaces the LCS keeps is arbitrary and
        // visually identical, so compare the substance.
        assert_eq!(
            new[n[0].start as usize..n[0].end as usize].trim(),
            "|| h.budget.Exhausted()"
        );
    }

    #[test]
    fn intraline_handles_a_substitution_on_both_sides() {
        let (o, n) = intraline("go ext.Run(ev)", "go ext.Submit(ev)");
        assert_eq!(o.len(), 1);
        assert_eq!(
            &"go ext.Run(ev)"[o[0].start as usize..o[0].end as usize],
            "Run"
        );
        assert_eq!(n.len(), 1);
        assert_eq!(
            &"go ext.Submit(ev)"[n[0].start as usize..n[0].end as usize],
            "Submit"
        );
    }

    #[test]
    fn a_rewritten_phrase_highlights_as_one_block() {
        // Every space the LCS matched used to punch a hole in the highlight, so
        // a rewritten comment came out as a row of separate blocks.
        let old = "# Collect the failures first";
        let new = "# Collect every check failure before exiting";
        let (_, n) = intraline(old, new);
        assert_eq!(
            n.len(),
            1,
            "expected one block, got {:?}",
            n.iter()
                .map(|s| &new[s.start as usize..s.end as usize])
                .collect::<Vec<_>>()
        );
        assert_eq!(
            &new[n[0].start as usize..n[0].end as usize],
            "every check failure before exiting"
        );
    }

    #[test]
    fn unrelated_lines_that_happen_to_pair_get_no_highlighting() {
        // Straight off a screenshot: one removed line, three added, so the
        // position-matched pair was a shell command against a comment. Marking
        // the whole comment as "changed words" reports a rewrite that never
        // happened and buries the text under the highlight background.
        let (o, n) = intraline(
            "    - bash cicd/pipeline/run-all-checks.sh;",
            "    # Collect every check failure before exiting so one bad check does",
        );
        assert!(o.is_empty() && n.is_empty(), "{o:?} {n:?}");
    }

    #[test]
    fn a_genuine_rewrite_still_highlights_at_the_measured_floor() {
        // The least similar real pair in the fixtures, at 0.60. If the floor
        // ever rises past this, actual renames stop being highlighted.
        let (o, n) = intraline(
            "#define ZIG_DECL AUTO_EXTERN_C_ZIG",
            "#define RUST_DECL AUTO_EXTERN_C_RUST",
        );
        assert!(!o.is_empty() && !n.is_empty());
        // Both identifiers changed and only a space separates them, so they
        // coalesce into one block — `AUTO_EXTERN_C_ZIG` is a single token, not a
        // shared prefix with a different tail.
        let new = "#define RUST_DECL AUTO_EXTERN_C_RUST";
        assert_eq!(
            &new[n[0].start as usize..n[0].end as usize],
            "RUST_DECL AUTO_EXTERN_C_RUST"
        );
    }

    #[test]
    fn coalescing_never_swallows_unchanged_words() {
        // Only whitespace gaps close. A real word between two changed ones stays
        // outside the highlight, which is the whole point of an intraline diff.
        let (_, n) = intraline("a keep b", "x keep y");
        assert_eq!(n.len(), 2, "{:?}", n);
        assert_eq!(&"x keep y"[n[0].start as usize..n[0].end as usize], "x");
        assert_eq!(&"x keep y"[n[1].start as usize..n[1].end as usize], "y");
    }

    #[test]
    fn replace_pairs_matches_deletes_to_adds() {
        let raw = "\
diff --git a/x b/x
@@ -1,3 +1,3 @@
 keep
-old one
-old two
+new one
+new two
 tail
";
        let f = parse_unified_diff(raw);
        let pairs = replace_pairs(&f[0].hunks[0]);
        assert_eq!(pairs, vec![(1, 3), (2, 4)]);
    }

    #[test]
    fn intraline_bails_out_on_machine_generated_lines() {
        // A minified bundle or base64 blob. Without the guard this allocates an
        // LCS table of tokens^2 and takes the process down.
        let huge_a = "x ".repeat(MAX_INTRALINE_TOKENS + 50);
        let huge_b = "y ".repeat(MAX_INTRALINE_TOKENS + 50);
        let (o, n) = intraline(&huge_a, &huge_b);
        assert!(
            o.is_empty() && n.is_empty(),
            "must degrade to no highlighting"
        );

        // Just under the cap still works normally.
        let ok_a = "a ".repeat(100);
        let ok_b = format!("{}b", "a ".repeat(100));
        let (_, n) = intraline(&ok_a, &ok_b);
        assert!(!n.is_empty(), "normal lines must still be diffed");
    }
}
