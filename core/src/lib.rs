//! Everything here is pure: no GPUI, no gitoxide, no I/O.
//!
//! That is the whole point. If the shell has to be rewritten — GPUI to Electron,
//! or the other way — nothing in this crate changes. Keep it that way: the day
//! something in here needs to know what a window is, the boundary is gone.

pub mod host;
pub mod prepared;
pub mod syntax;
pub mod theme;

// ---------------------------------------------------------------- commit log

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub parents: Vec<String>,
    pub author: String,
    pub timestamp: i64,
    pub subject: String,
}

/// Parses the output of `fixtures/dump.sh` (see that script for the format).
/// Fields are \x1f-separated, records \x1e-separated — control characters git
/// will never emit inside a subject, so there is nothing to escape.
pub fn parse_log(raw: &str) -> Vec<Commit> {
    raw.split('\u{1e}')
        .map(str::trim)
        .filter(|rec| !rec.is_empty())
        .filter_map(|rec| {
            let f: Vec<&str> = rec.split('\u{1f}').collect();
            if f.len() < 6 {
                return None;
            }
            Some(Commit {
                sha: f[0].to_string(),
                short: f[1].to_string(),
                parents: f[2].split_whitespace().map(str::to_string).collect(),
                author: f[3].to_string(),
                timestamp: f[4].parse().unwrap_or(0),
                subject: f[5].to_string(),
            })
        })
        .collect()
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
    // lanes[i] = the sha lane `i` is currently waiting to draw.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for c in commits {
        let lane = match lanes.iter().position(|l| l.as_deref() == Some(&c.sha)) {
            Some(i) => i,
            None => claim_lane(&mut lanes, &c.sha),
        };

        // Any *other* lane waiting on this sha converges here.
        let mut merges = Vec::new();
        for (i, l) in lanes.iter_mut().enumerate() {
            if i != lane && l.as_deref() == Some(&c.sha) {
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
        lanes[lane] = parents.next().cloned();
        let forks: Vec<usize> = parents.map(|p| claim_lane(&mut lanes, p)).collect();

        rows.push(GraphRow { lane, through, merges, forks });
    }
    rows
}

fn claim_lane(lanes: &mut Vec<Option<String>>, sha: &str) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => {
            lanes[i] = Some(sha.to_string());
            i
        }
        None => {
            lanes.push(Some(sha.to_string()));
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
    pub text: String,
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

/// Parses `git diff` unified output. Enough for the spike; binary files,
/// renames and mode changes are skipped rather than modelled.
pub fn parse_unified_diff(raw: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut old_no, mut new_no) = (0u32, 0u32);

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest
                .split_whitespace()
                .nth(1)
                .and_then(|p| p.strip_prefix("b/"))
                .unwrap_or("?")
                .to_string();
            files.push(FileDiff { path, hunks: Vec::new() });
            continue;
        }
        if line.starts_with("@@ ") {
            let (o, n) = parse_hunk_header(line);
            old_no = o;
            new_no = n;
            if let Some(f) = files.last_mut() {
                f.hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
            }
            continue;
        }
        // Skip the metadata lines that would otherwise look like +/- content.
        if line.starts_with("+++ ") || line.starts_with("--- ") || line.starts_with("index ") {
            continue;
        }
        let Some(hunk) = files.last_mut().and_then(|f| f.hunks.last_mut()) else {
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
        hunk.lines.push(DiffLine { kind, old_no: o, new_no: n, text: text.to_string() });
    }
    files
}

/// `@@ -41,9 +41,11 @@ ...` -> (41, 41)
fn parse_hunk_header(line: &str) -> (u32, u32) {
    let mut old = 0;
    let mut new = 0;
    for tok in line.split_whitespace() {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// Splits a line into word-ish tokens: runs of alphanumerics/underscore, and
/// every other character on its own. Word granularity, not character — a
/// char-level diff of code highlights every bracket and reads as confetti.
fn tokenize(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
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
        out.push((start, &line[start..i]));
    }
    out
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
    let a = tokenize(old);
    let b = tokenize(new);

    if a.len() > MAX_INTRALINE_TOKENS || b.len() > MAX_INTRALINE_TOKENS {
        return (Vec::new(), Vec::new());
    }

    // Classic LCS table over tokens.
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i].1 == b[j].1 {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    // The table's corner is the length of the longest common subsequence, so
    // the similarity of the pair is already paid for.
    let common = lcs[0][0];
    let similarity = 2.0 * common as f32 / (a.len() + b.len()) as f32;
    if similarity < MIN_INTRALINE_SIMILARITY {
        return (Vec::new(), Vec::new());
    }

    let (mut old_spans, mut new_spans) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].1 == b[j].1 {
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            push_span(&mut old_spans, a[i].0, a[i].0 + a[i].1.len());
            i += 1;
        } else {
            push_span(&mut new_spans, b[j].0, b[j].0 + b[j].1.len());
            j += 1;
        }
    }
    while i < a.len() {
        push_span(&mut old_spans, a[i].0, a[i].0 + a[i].1.len());
        i += 1;
    }
    while j < b.len() {
        push_span(&mut new_spans, b[j].0, b[j].0 + b[j].1.len());
        j += 1;
    }
    coalesce(&mut old_spans, old);
    coalesce(&mut new_spans, new);
    (old_spans, new_spans)
}

/// Merge into the previous span when adjacent, so a changed phrase highlights
/// as one block rather than a row of separate token boxes.
fn push_span(spans: &mut Vec<Span>, start: usize, end: usize) {
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
                    .get(last.end..s.start)
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
/// A run of N removals immediately followed by M additions pairs index-wise;
/// unmatched leftovers are pure adds or deletes and get no highlighting.
pub fn replace_pairs(hunk: &Hunk) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut i = 0;
    let lines = &hunk.lines;
    while i < lines.len() {
        if lines[i].kind != LineKind::Removed {
            i += 1;
            continue;
        }
        let del_start = i;
        while i < lines.len() && lines[i].kind == LineKind::Removed {
            i += 1;
        }
        let add_start = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        let n = (add_start - del_start).min(i - add_start);
        for k in 0..n {
            pairs.push((del_start + k, add_start + k));
        }
    }
    pairs
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

    #[test]
    fn log_round_trips() {
        let raw = "abc\u{1f}abc\u{1f}def ghi\u{1f}Ada\u{1f}1700000000\u{1f}Fix the thing\u{1e}\
                   def\u{1f}def\u{1f}\u{1f}Ada\u{1f}1699999999\u{1f}Initial commit\u{1e}";
        let c = parse_log(raw);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].parents, vec!["def", "ghi"]);
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
        assert_eq!(initials("émile"), "ÉM", "a multi-byte first letter is still one letter");
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
    fn intraline_marks_only_the_changed_tokens() {
        let old = "if !ext.Handles(ev.Kind) {";
        let new = "if !ext.Handles(ev.Kind) || h.budget.Exhausted() {";
        let (o, n) = intraline(old, new);
        assert!(o.is_empty(), "nothing was removed, got {:?}", o);
        assert_eq!(n.len(), 1, "the inserted clause should be one merged span");
        // Which of the two equivalent spaces the LCS keeps is arbitrary and
        // visually identical, so compare the substance.
        assert_eq!(new[n[0].start..n[0].end].trim(), "|| h.budget.Exhausted()");
    }

    #[test]
    fn intraline_handles_a_substitution_on_both_sides() {
        let (o, n) = intraline("go ext.Run(ev)", "go ext.Submit(ev)");
        assert_eq!(o.len(), 1);
        assert_eq!(&"go ext.Run(ev)"[o[0].start..o[0].end], "Run");
        assert_eq!(n.len(), 1);
        assert_eq!(&"go ext.Submit(ev)"[n[0].start..n[0].end], "Submit");
    }

    #[test]
    fn a_rewritten_phrase_highlights_as_one_block() {
        // Every space the LCS matched used to punch a hole in the highlight, so
        // a rewritten comment came out as a row of separate blocks.
        let old = "# Collect the failures first";
        let new = "# Collect every check failure before exiting";
        let (_, n) = intraline(old, new);
        assert_eq!(n.len(), 1, "expected one block, got {:?}", n
            .iter()
            .map(|s| &new[s.start..s.end])
            .collect::<Vec<_>>());
        assert_eq!(&new[n[0].start..n[0].end], "every check failure before exiting");
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
        let (o, n) = intraline("#define ZIG_DECL AUTO_EXTERN_C_ZIG", "#define RUST_DECL AUTO_EXTERN_C_RUST");
        assert!(!o.is_empty() && !n.is_empty());
        // Both identifiers changed and only a space separates them, so they
        // coalesce into one block — `AUTO_EXTERN_C_ZIG` is a single token, not a
        // shared prefix with a different tail.
        let new = "#define RUST_DECL AUTO_EXTERN_C_RUST";
        assert_eq!(&new[n[0].start..n[0].end], "RUST_DECL AUTO_EXTERN_C_RUST");
    }

    #[test]
    fn coalescing_never_swallows_unchanged_words() {
        // Only whitespace gaps close. A real word between two changed ones stays
        // outside the highlight, which is the whole point of an intraline diff.
        let (_, n) = intraline("a keep b", "x keep y");
        assert_eq!(n.len(), 2, "{:?}", n);
        assert_eq!(&"x keep y"[n[0].start..n[0].end], "x");
        assert_eq!(&"x keep y"[n[1].start..n[1].end], "y");
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
        assert!(o.is_empty() && n.is_empty(), "must degrade to no highlighting");

        // Just under the cap still works normally.
        let ok_a = "a ".repeat(100);
        let ok_b = format!("{}b", "a ".repeat(100));
        let (_, n) = intraline(&ok_a, &ok_b);
        assert!(!n.is_empty(), "normal lines must still be diffed");
    }
}
