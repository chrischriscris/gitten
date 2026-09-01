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

use crate::{Hunk, LineKind};

/// The unified diff that applies exactly `chosen` — nothing around them.
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
            body.push(b'\n');
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
}
