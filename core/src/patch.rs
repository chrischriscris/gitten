//! A patch for exactly the hunks the reader chose.
//!
//! Staging one hunk of a file is a text problem before it is a git problem:
//! the hunk already carries every line that has to travel — its removals, its
//! additions and the unchanged lines around them — so what is left is to wrap
//! those lines in a valid unified diff and let `git apply` do the aiming.
//! This module is that wrapping, and nothing else: no I/O, no process, no
//! repository. The verbs that feed the result to git live behind
//! [`crate::Repo::stage_patch`] in `gitten-git` and its siblings.
//!
//! # Where the numbers come from
//!
//! A [`Hunk`]'s own header describes the whole diff between the two sides, so
//! staging it verbatim would stage its neighbours too. The header here is
//! therefore **recomputed from the lines themselves**: each [`DiffLine`]
//! carries both sides' line numbers, so the coordinate is the first line that
//! lives on that side and each count is a scan over the kinds. Nothing is
//! remembered and nothing is guessed, and a hunk assembled by any
//! [`Differ`](crate::differ::Differ) — including one an extension registered —
//! synthesizes correctly by the same rule the view draws it by.
//!
//! # What cannot be said yet
//!
//! A `\ No newline at end of file` marker needs a fact the line model does
//! not carry: whether either side's final line was newline-terminated.
//! Acquisition splits content into lines and the terminator goes with it
//! (see `gitten_git::lines`), so this module cannot tell `a\nb\n` from
//! `a\nb`. Every line is therefore emitted *as if* terminated, which is the
//! common case and byte-correct for it; a hunk touching a file that lacks
//! the final newline produces a patch `git apply` refuses rather than
//! misapplies, and the refusal surfaces verbatim where the verb's error
//! goes — honest in exactly the way silent corruption would not be. Closing
//! the gap is a line-model change (`Option<bool>` on the pair, threaded
//! through acquisition), not a change here.

use crate::{DiffLine, Hunk, LineKind};

/// What a `\ No newline at end of file` marker needs, per side: how many
/// lines the side holds, and whether its last one is newline-terminated.
///
/// Neither fact lives in a [`Hunk`] — a hunk's lines are text without their
/// terminators — so the caller that read the content supplies them: the
/// counts are each side's line count and the booleans are whether the raw
/// bytes ended in `\n`. A patch that never reaches a side's final line pays
/// for none of this; the marker is written only under a line that *is* the
/// side's last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sides {
    /// How many lines the old side holds.
    pub old_lines: usize,
    /// Whether the old side's final line is newline-terminated.
    pub old_final_newline: bool,
    /// How many lines the new side holds.
    pub new_lines: usize,
    /// Whether the new side's final line is newline-terminated.
    pub new_final_newline: bool,
}

impl Sides {
    /// [`emit`]'s historical reading: both sides as if terminated, so no
    /// marker is ever written. Exactly right for content that ends in a
    /// newline, and — for content that does not — the shape `git apply`
    /// refuses rather than misapplies.
    pub const fn unbounded() -> Self {
        Self {
            old_lines: usize::MAX,
            old_final_newline: true,
            new_lines: usize::MAX,
            new_final_newline: true,
        }
    }
}

/// The unified diff that applies exactly `chosen` — nothing around them.
///
/// [`emit_with`] is the general form and carries the sides' final-line
/// facts; this is the historical spelling, which assumes every side is
/// newline-terminated and writes no marker. A patch built from content
/// that lacks the final newline is refused by `git apply` verbatim rather
/// than misapplied — the caller that knows the sides should prefer
/// [`emit_with`] and say it properly.
///
/// One file per call, because that is what a hunk belongs to; several hunks
/// of that file ride together as one patch, which keeps a future multi-hunk
/// selection a wider slice away rather than a redesign. The path is the
/// diff's own label for the file, spelled under both `a/` and `b/`.
///
/// Empty in, empty out: no chosen hunks, or only hunks without lines,
/// yields no bytes. An empty patch applies nothing, so callers refuse it
/// before anything runs — the refusal is theirs to word, because "the
/// keyboard is not on a hunk" is a sentence about the screen and this
/// module never sees the screen.
///
/// The bytes are UTF-8 by construction: the path arrived through the lossy
/// decode every diff takes, and the lines are shared handles out of it.
pub fn emit(path: &str, chosen: &[&Hunk]) -> Vec<u8> {
    emit_inner(path, chosen, &Sides::unbounded())
}

/// [`emit`], with the sides' final-line facts: a line that is its side's
/// last and is not newline-terminated is written the way git itself writes
/// it — bare, followed by `\\ No newline at end of file` — so a patch
/// against content that lacks the final newline *applies* instead of being
/// refused.
///
/// One shape cannot be said and is refused rather than faked: a context
/// line on which the two sides disagree about the terminator — the old
/// side's final line without a newline that the new side carries on
/// past. The line model sees one text where the bytes differ, the marker
/// would have to be written and not written at once, and the honest answer
/// is the refusal: a partial patch cannot say both sides at once, and the
/// whole-file door can.
pub fn emit_with(path: &str, chosen: &[&Hunk], sides: &Sides) -> Result<Vec<u8>, String> {
    // The disagreement lives in the chosen lines, so it is cheap to see
    // before anything is built: a context line that is its old side's last
    // must also be its new side's last (or neither), whenever a marker
    // would be involved.
    for hunk in chosen {
        for l in &hunk.lines {
            if l.kind != LineKind::Context {
                continue;
            }
            let old_final = l.old_no.is_some_and(|n| n as usize == sides.old_lines);
            let new_final = l.new_no.is_some_and(|n| n as usize == sides.new_lines);
            let old_marked = old_final && !sides.old_final_newline;
            let new_marked = new_final && !sides.new_final_newline;
            if old_marked != new_marked {
                return Err(format!(
                    "{path} changes its final newline here — a partial patch cannot say both sides at once; stage or discard it whole from the files pane"
                ));
            }
        }
    }
    Ok(emit_inner(path, chosen, sides))
}

fn emit_inner(path: &str, chosen: &[&Hunk], sides: &Sides) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    // The sides are decided across the whole selection, not per hunk: two
    // chosen hunks of a brand-new file must agree there is no old side.
    let mut any_old = false;
    let mut any_new = false;
    for hunk in chosen {
        if hunk.lines.is_empty() {
            continue;
        }
        let (old_count, new_count) = counts(hunk);
        any_old |= old_count > 0;
        any_new |= new_count > 0;
        body.extend_from_slice(coords(hunk).as_bytes());
        for l in &hunk.lines {
            body.push(match l.kind {
                LineKind::Context => b' ',
                LineKind::Added => b'+',
                LineKind::Removed => b'-',
            });
            body.extend_from_slice(l.text.as_bytes());
            // The marker replaces the terminator, exactly as git writes
            // it: the line bare, then the marker line. Only a line that
            // *is* its side's last can carry one — any other line is
            // followed by more content and terminated like any other.
            let final_of_side =
                |no: Option<u32>, total: usize| no.is_some_and(|n| n as usize == total);
            let marked = match l.kind {
                LineKind::Context => {
                    final_of_side(l.old_no, sides.old_lines) && !sides.old_final_newline
                }
                LineKind::Added => {
                    final_of_side(l.new_no, sides.new_lines) && !sides.new_final_newline
                }
                LineKind::Removed => {
                    final_of_side(l.old_no, sides.old_lines) && !sides.old_final_newline
                }
            };
            if marked {
                body.extend_from_slice(b"\n\\ No newline at end of file\n");
            } else {
                body.push(b'\n');
            }
        }
    }
    if body.is_empty() {
        return body;
    }

    // The file half of the header. `git apply` reads the paths off these two
    // lines; the `diff --git` line ahead of them is convention, kept because
    // it is what git itself writes and costs nothing.
    //
    // `/dev/null` on a side with no lines is not decoration — it is how a
    // patch says *this side does not exist*, which is what turns a selection
    // of additions into a file creation and a selection of removals into a
    // deletion when the index or the worktree is on the receiving end. The
    // `diff --git` line keeps both names whatever the sides say, as git's
    // own output does.
    let mut out = Vec::with_capacity(body.len() + path.len() * 2 + 64);
    let (old_name, new_name) = match (any_old, any_new) {
        (true, true) => (format!("a/{path}"), format!("b/{path}")),
        (false, _) => ("/dev/null".to_string(), format!("b/{path}")),
        (true, false) => (format!("a/{path}"), "/dev/null".to_string()),
    };
    out.extend_from_slice(format!("diff --git a/{path} b/{path}\n").as_bytes());
    out.extend_from_slice(format!("--- {old_name}\n+++ {new_name}\n").as_bytes());
    out.extend_from_slice(&body);
    out
}

/// How many drawn lines belong to each side of one hunk's header:
/// `(old, new)`. A context line lives on both; an addition only on the new;
/// a removal only on the old.
fn counts(hunk: &Hunk) -> (usize, usize) {
    let mut n = (0usize, 0usize);
    for l in &hunk.lines {
        match l.kind {
            LineKind::Context => n = (n.0 + 1, n.1 + 1),
            LineKind::Added => n.1 += 1,
            LineKind::Removed => n.0 += 1,
        }
    }
    n
}

/// The `@@ -a,b +c,d @@` line, recomputed against the lines themselves.
///
/// The coordinates are git's own printed form: the start is the number of
/// the first line living on that side (one-based, as every `DiffLine`
/// carries them), a count of one is spelled bare, and an empty side spells
/// `0,0` — the shape every whole-file creation and deletion carries, because
/// those are the only selections that can empty a side out.
fn coords(hunk: &Hunk) -> String {
    let mut old = None;
    let mut new = None;
    for l in &hunk.lines {
        old = old.or(l.old_no);
        new = new.or(l.new_no);
    }
    let (o_count, n_count) = counts(hunk);
    let side = |first: Option<u32>, count: usize| match count {
        0 => "0,0".to_string(),
        1 => format!("{}", first.unwrap_or(0)),
        n => format!("{},{}", first.unwrap_or(0), n),
    };
    format!("@@ -{} +{} @@\n", side(old, o_count), side(new, n_count))
}

/// Which unchosen changes a line window keeps, and which it drops.
///
/// The verbs aim at different trees, and the same span means different
/// things to each — and the direction of `git apply` decides which side of
/// the patch the patch is matched against. A stage is forward: the
/// patch's preimage is the index, which holds every removal, chosen or
/// not, so an unchosen removal travels as context (genuinely on both
/// sides: in the index, and in the index the stage produces) while an
/// unchosen addition is on neither side and drops out. An unstage and a
/// discard are reverse: `git apply --reverse` matches the patch's *post*
/// image against the tree being rewritten, which holds every addition —
/// so an unchosen addition travels as context and an unchosen removal, on
/// neither side, drops out. Either way the patch describes a real pair of
/// texts, which is what keeps `git apply` willing: a hunk whose preimage
/// skips a line the file has, or that ends a mid-file hunk on a removal,
/// is a patch git refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unselected {
    /// Staging: unchosen removals stay (as context), unchosen additions drop.
    KeepRemovals,
    /// Unstaging and discarding — the reverse verbs: unchosen additions
    /// stay (as context), unchosen removals drop.
    KeepAdditions,
}

/// The hunk that stages exactly the marked rows of one hunk: the marked
/// window `lo..=hi`, widened by up to three context rows on each side —
/// widening that walks past an unchosen change to reach it.
///
/// Three rules make the window appliable and nothing more. Every row
/// *inside* the marked window travels as the change it is — a range stages
/// all the changes it spans, which is what a dragged selection means. The
/// widening takes context rows only, up to three, and counts past an
/// unchosen change without taking it — the unchosen lines it steps over
/// are then ruled by `keep`: kept as context where both sides of the
/// patch genuinely hold them, dropped where neither does. And the header
/// is left empty: [`emit_with`] recomputes coordinates from the lines, so
/// a slice is addressable wherever it falls.
///
/// `None` means the window stages nothing — an empty range, a range past
/// the hunk's end, or every line in it context — and the caller says so;
/// "nothing selected" is a sentence about the screen.
pub fn line_window(hunk: &Hunk, lo: usize, hi: usize, keep: Unselected) -> Option<Hunk> {
    let lines = &hunk.lines;
    if lo > hi || hi >= lines.len() {
        return None;
    }
    let mut start = lo;
    let mut taken = 0;
    while start > 0 && taken < 3 {
        match lines[start - 1].kind {
            LineKind::Context => {
                start -= 1;
                taken += 1;
            }
            // An unchosen change is stepped over, not taken: it is ruled
            // by `keep` below, and it does not spend the context budget.
            _ => start -= 1,
        }
    }
    let mut end = hi;
    taken = 0;
    while end + 1 < lines.len() && taken < 3 {
        match lines[end + 1].kind {
            LineKind::Context => {
                end += 1;
                taken += 1;
            }
            _ => end += 1,
        }
    }
    let mut out: Vec<DiffLine> = Vec::with_capacity(end - start + 1);
    let mut changed = false;
    for (i, l) in lines.iter().enumerate().skip(start).take(end - start + 1) {
        let chosen = (lo..=hi).contains(&i);
        let kind = match (l.kind, chosen) {
            (LineKind::Context, _) | (_, true) => l.kind,
            (LineKind::Added, false) => match keep {
                Unselected::KeepRemovals => continue,
                Unselected::KeepAdditions => LineKind::Context,
            },
            (LineKind::Removed, false) => match keep {
                Unselected::KeepRemovals => LineKind::Context,
                Unselected::KeepAdditions => continue,
            },
        };
        changed |= kind != LineKind::Context;
        let mut line = l.clone();
        line.kind = kind;
        out.push(line);
    }
    changed.then(|| Hunk {
        header: String::new(),
        lines: out,
    })
}

// ---------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_unified_diff;

    /// A hunk straight from the parser, so tests exercise the shapes real
    /// diffs arrive in rather than hand-built structs.
    fn one(raw: &str) -> Hunk {
        let files = parse_unified_diff(raw);
        assert_eq!(files.len(), 1, "test fixture holds one file");
        files[0]
            .hunks
            .first()
            .cloned()
            .expect("fixture holds one hunk")
    }

    fn text(patch: &[u8]) -> String {
        String::from_utf8(patch.to_vec()).expect("a synthesized patch is UTF-8")
    }

    #[test]
    fn a_modification_hunk_synthesizes_whole_with_its_context() {
        let hunk = one("\
diff --git a/f.txt b/f.txt
--- a/f.txt
+++ b/f.txt
@@ -1,3 +1,3 @@
 keep
-was
+now
 tail
");
        assert_eq!(
            text(&emit("f.txt", &[&hunk])),
            "\
diff --git a/f.txt b/f.txt
--- a/f.txt
+++ b/f.txt
@@ -1,3 +1,3 @@
 keep
-was
+now
 tail
"
        );
    }

    #[test]
    fn the_header_is_recomputed_not_carried() {
        // The source hunk drew six rows under a `-41,4 +41,5` header whose
        // counts describe the whole diff's shape. Staged alone, the header
        // must come off the lines — three lines live on the old side, five
        // on the new — never off the header the view happened to draw.
        let hunk = one("\
diff --git a/big.rs b/big.rs
@@ -41,4 +41,5 @@ fn dispatch() {
 \tlet a = 1;
-\tgo(a);
+\tif a > 0 {
+\t\tgo(a);
+\t}
 \tlet b = 2;
");
        assert_eq!(
            text(&emit("big.rs", &[&hunk])),
            "\
diff --git a/big.rs b/big.rs
--- a/big.rs
+++ b/big.rs
@@ -41,3 +41,5 @@
 \tlet a = 1;
-\tgo(a);
+\tif a > 0 {
+\t\tgo(a);
+\t}
 \tlet b = 2;
"
        );
    }

    #[test]
    fn a_count_of_one_prints_bare() {
        // git's convention: a single-line side spells no count. `git apply`
        // accepts both spellings, but matching its output exactly is what
        // makes comparisons against real patches readable.
        let hunk = one("\
diff --git a/x.txt b/x.txt
@@ -2 +2 @@
-was
+now
");
        assert_eq!(
            text(&emit("x.txt", &[&hunk])),
            "\
diff --git a/x.txt b/x.txt
--- a/x.txt
+++ b/x.txt
@@ -2 +2 @@
-was
+now
"
        );
    }

    #[test]
    fn two_chosen_hunks_ride_as_one_patch_and_one_rides_alone() {
        let files = parse_unified_diff(
            "\
diff --git a/two.txt b/two.txt
--- a/two.txt
+++ b/two.txt
@@ -1,3 +1,3 @@
 one
-was one
+now one
 two
@@ -10,3 +10,3 @@
 nine
-was ten
+now ten
 eleven
",
        );
        assert_eq!(files[0].hunks.len(), 2);

        let second = &files[0].hunks[1];
        assert_eq!(
            text(&emit("two.txt", &[second])),
            "\
diff --git a/two.txt b/two.txt
--- a/two.txt
+++ b/two.txt
@@ -10,3 +10,3 @@
 nine
-was ten
+now ten
 eleven
",
            "only the chosen hunk travels"
        );

        let both = [&files[0].hunks[0], &files[0].hunks[1]];
        assert_eq!(
            text(&emit("two.txt", &both)),
            "\
diff --git a/two.txt b/two.txt
--- a/two.txt
+++ b/two.txt
@@ -1,3 +1,3 @@
 one
-was one
+now one
 two
@@ -10,3 +10,3 @@
 nine
-was ten
+now ten
 eleven
"
        );
    }

    #[test]
    fn a_brand_new_file_names_dev_null_on_the_old_side() {
        // Every line is an addition: there was nothing before, and the patch
        // says so twice — once in `/dev/null`, once in `-0,0`.
        let hunk = one("\
diff --git a/new.txt b/new.txt
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+first
+second
");
        assert_eq!(
            text(&emit("new.txt", &[&hunk])),
            "\
diff --git a/new.txt b/new.txt
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+first
+second
"
        );
    }

    #[test]
    fn a_whole_file_deletion_names_dev_null_on_the_new_side() {
        let hunk = one("\
diff --git a/gone.txt b/gone.txt
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-first
-second
");
        assert_eq!(
            text(&emit("gone.txt", &[&hunk])),
            "\
diff --git a/gone.txt b/gone.txt
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-first
-second
"
        );
    }

    #[test]
    fn a_synthesized_patch_parses_back_to_the_lines_it_came_from() {
        // The property everything downstream rests on: synthesis and parsing
        // agree. If the emitted patch ever read back differently from the
        // hunk it was built from, `git apply` would be aimed at something
        // nobody chose.
        let files = parse_unified_diff(
            "\
diff --git a/rt.txt b/rt.txt
@@ -3,7 +3,7 @@
 keep
-drop me
+kept instead
 more
-gone
+here
 tail
 tail two
 tail three
",
        );
        let hunk = files[0].hunks.first().unwrap().clone();
        let again = parse_unified_diff(&text(&emit("rt.txt", &[&hunk])));
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].path, "rt.txt");
        assert_eq!(again[0].hunks.len(), 1);
        assert_eq!(again[0].hunks[0], hunk);
    }

    #[test]
    fn hunks_from_the_differ_itself_synthesize() {
        // Not just parsed hunks: the pipeline's own product — edits from a
        // real differ, assembled by `differ::hunks` — has to survive the
        // trip, because that is the shape the view actually hands over.
        use crate::differ::{self, Differ, Histogram};
        use std::sync::Arc;
        let words = [
            "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
        ];
        let old: Vec<Arc<str>> = words.iter().map(|s| Arc::from(*s)).collect();
        let new: Vec<Arc<str>> = words
            .iter()
            .enumerate()
            .map(|(i, s)| match i {
                1 => Arc::<str>::from("TWO"),
                7 => Arc::<str>::from("EIGHT"),
                _ => Arc::from(*s),
            })
            .collect();
        let edits = Histogram::default().diff("f", &old, &new);
        let hunks = differ::hunks(&old, &new, &edits, 1);
        assert_eq!(hunks.len(), 2, "edits far apart stay two hunks");

        for chosen in [vec![&hunks[0]], vec![&hunks[1]], vec![&hunks[0], &hunks[1]]] {
            let patch = emit("f", &chosen);
            let again = parse_unified_diff(&text(&patch));
            assert_eq!(again.len(), 1);
            assert_eq!(again[0].hunks.len(), chosen.len());
            for (got, want) in again[0].hunks.iter().zip(&chosen) {
                assert_eq!(got.lines, want.lines, "round-trip of {patch:?}");
                // And the content really is the edit, not just its shape.
                assert!(
                    got.lines
                        .iter()
                        .any(|l| *l.text == *"TWO" || *l.text == *"EIGHT"),
                    "{patch:?}"
                );
            }
        }
    }

    #[test]
    fn nothing_chosen_is_no_bytes_at_all() {
        assert!(emit("f.txt", &[]).is_empty(), "an empty selection is quiet");
        // A hunk without lines — beside the binary placeholder's empty
        // hunk list — contributes nothing rather than a bare header pair.
        let empty = Hunk {
            header: "@@ -1 +1 @@".into(),
            lines: Vec::new(),
        };
        assert!(emit("f.txt", &[&empty]).is_empty());
    }

    #[test]
    fn a_carriage_return_that_is_content_travels_in_the_patch() {
        // CRLF endings live inside the line as `\r`; the terminator this
        // module adds is the patch's own `\n`. Losing the `\r` here would
        // stage the file with its endings silently rewritten.
        let hunk = one("\
diff --git a/w.txt b/w.txt
@@ -1,2 +1,2 @@
-alpha\r
+beta
 keep
");
        let patch = text(&emit("w.txt", &[&hunk]));
        assert!(patch.contains("-alpha\r\n"), "the CR rode along as content");
        assert!(patch.contains("+beta\n"), "and the plain line stayed plain");
    }

    // ------------------------------------------------------ line windows

    /// A replacement hunk: context, a removal paired with an addition,
    /// context — the shape every line-selection question is asked in.
    fn replacement() -> Hunk {
        one("\
diff --git a/f.txt b/f.txt
@@ -1,5 +1,5 @@
 alpha
-STAGED CHANGE
+WORKTREE CHANGE
 keep three
 omega
")
    }

    #[test]
    fn a_window_widens_by_context_and_steps_over_an_unchosen_change() {
        let hunk = replacement();
        // Selecting the removal alone: the unchosen addition is stepped
        // over to reach the context beyond it, and what happens to it is
        // the verb's word.
        let plus = hunk
            .lines
            .iter()
            .position(|l| l.kind == LineKind::Removed)
            .expect("a removal");
        let staged =
            line_window(&hunk, plus, plus, Unselected::KeepRemovals).expect("a changed line");
        let kinds: Vec<LineKind> = staged.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Context,
                LineKind::Removed,
                LineKind::Context,
                LineKind::Context
            ],
            "staging keeps the unchosen removal as context and drops the addition: {kinds:?}"
        );
        let discarded =
            line_window(&hunk, plus, plus, Unselected::KeepAdditions).expect("a changed line");
        let kinds: Vec<LineKind> = discarded.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Context,
                LineKind::Removed,
                LineKind::Context,
                LineKind::Context,
                LineKind::Context
            ],
            "discarding keeps the unchosen addition as context: {kinds:?}"
        );
    }

    #[test]
    fn a_window_of_every_line_is_the_hunk_itself() {
        let hunk = replacement();
        let whole = line_window(&hunk, 0, hunk.lines.len() - 1, Unselected::KeepRemovals)
            .expect("a changed line");
        assert_eq!(
            whole.lines, hunk.lines,
            "nothing unchosen, nothing rewritten"
        );
    }

    #[test]
    fn a_window_over_context_alone_stages_nothing() {
        let hunk = replacement();
        assert!(line_window(&hunk, 0, 0, Unselected::KeepRemovals).is_none());
        assert!(line_window(&hunk, 99, 100, Unselected::KeepRemovals).is_none());
        assert!(line_window(&hunk, 2, 1, Unselected::KeepRemovals).is_none());
    }

    // ------------------------------------------------- final-newline facts

    #[test]
    fn the_marker_rides_each_sides_own_last_line() {
        // Both sides end without the newline: the removal and the addition
        // each carry the marker git itself would write.
        let hunk = one("\
diff --git a/f.txt b/f.txt
@@ -1,2 +1,2 @@
 alpha
-end
+END
");
        let sides = Sides {
            old_lines: 2,
            old_final_newline: false,
            new_lines: 2,
            new_final_newline: false,
        };
        let patch = text(&emit_with("f.txt", &[&hunk], &sides).expect("both sides agree"));
        assert!(
            patch.contains("-end\n\\ No newline at end of file\n"),
            "{patch}"
        );
        assert!(
            patch.contains("+END\n\\ No newline at end of file\n"),
            "{patch}"
        );
        // And the historical spelling — every line as if terminated — is
        // what plain `emit` still answers.
        assert!(
            !text(&emit("f.txt", &[&hunk])).contains("No newline"),
            "emit assumes terminators"
        );
    }

    #[test]
    fn the_unsayable_newline_change_refuses() {
        // The old side's last line becomes the new side's middle: context
        // in the line model, different bytes in the files, and no patch
        // that says both.
        let hunk = one("\
diff --git a/f.txt b/f.txt
@@ -1,2 +1,3 @@
 alpha
 end
+appended
");
        let sides = Sides {
            old_lines: 2,
            old_final_newline: false,
            new_lines: 3,
            new_final_newline: false,
        };
        let err = emit_with("f.txt", &[&hunk], &sides).expect_err("refuses");
        assert!(err.contains("final newline"), "{err}");
    }
}
