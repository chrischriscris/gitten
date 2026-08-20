//! A diff turned into rows, ready for whatever is going to draw them.
//!
//! Clipping, the intraline pass and the syntax pass are one job done in one
//! order, and every frontend needs the same result: the GPUI view, the ANSI
//! `paint` example, the headless `bench`, and a `cli/` that does not exist yet.
//! It lived in the view first and was immediately copied into two examples —
//! which is exactly what "don't put logic in `shell/` that `cli/` would have to
//! duplicate" is warning about, so it is here instead. What is left in a
//! frontend is drawing.
//!
//! Nothing in here knows what a window is. The one thing it takes from the
//! frontend is `max_line_chars`, because how wide a row may get is a rendering
//! budget rather than a fact about diffs.

use crate::syntax::{highlight_hunk, Highlighter, Token};
use crate::{intraline, replace_pairs, FileDiff, LineKind, Span};
use std::time::{Duration, Instant};

/// One line, with everything a renderer needs and nothing it would have to
/// recompute: the text as it will be shown, the words that changed inside it,
/// and its syntax tokens. Ranges index `text`, so they can never point past
/// what is on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
    pub spans: Vec<Span>,
    pub tokens: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub adds: usize,
    pub dels: usize,
    pub hunks: Vec<Hunk>,
}

/// The prepared diff, plus what the two expensive passes cost. The timings are
/// here because every frontend wants to report them and none of them should be
/// timing this themselves.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub files: Vec<File>,
    pub intraline: Duration,
    pub syntax: Duration,
}

impl Prepared {
    pub fn lines(&self) -> usize {
        self.files.iter().flat_map(|f| &f.hunks).map(|h| h.lines.len()).sum()
    }
}

/// Real repos contain minified bundles and base64 blobs; a single line of 9.6
/// million characters was measured in the wild. Text layout is linear in length,
/// so one such line stalls a frame. Nobody reads past column 2000 either way.
///
/// Clipping happens *before* both passes, so their output can only ever describe
/// text that will actually be drawn.
pub fn clip(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}  … {} more chars", n - max_chars)
}

pub fn prepare(files: &[FileDiff], hl: &dyn Highlighter, max_line_chars: usize) -> Prepared {
    let mut out = Vec::with_capacity(files.len());
    let mut intraline_time = Duration::ZERO;
    let mut syntax_time = Duration::ZERO;

    for f in files {
        let all = || f.hunks.iter().flat_map(|h| &h.lines);
        let adds = all().filter(|l| l.kind == LineKind::Added).count();
        let dels = all().filter(|l| l.kind == LineKind::Removed).count();
        let mut hunks = Vec::with_capacity(f.hunks.len());

        for h in &f.hunks {
            let mut texts: Vec<String> =
                h.lines.iter().map(|l| clip(&l.text, max_line_chars)).collect();

            // Second pass: only the removed/added pairs a line diff already
            // matched get word-level spans.
            let mut spans: Vec<Vec<Span>> = vec![Vec::new(); h.lines.len()];
            let t = Instant::now();
            for (d, a) in replace_pairs(h) {
                let (o, n) = intraline(&texts[d], &texts[a]);
                spans[d] = o;
                spans[a] = n;
            }
            intraline_time += t.elapsed();

            let t = Instant::now();
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let kinds: Vec<LineKind> = h.lines.iter().map(|l| l.kind).collect();
            let mut tokens = highlight_hunk(hl, &f.path, &refs, &kinds);
            syntax_time += t.elapsed();

            let lines = h
                .lines
                .iter()
                .enumerate()
                .map(|(i, l)| Line {
                    kind: l.kind,
                    old_no: l.old_no,
                    new_no: l.new_no,
                    text: std::mem::take(&mut texts[i]),
                    spans: std::mem::take(&mut spans[i]),
                    tokens: std::mem::take(&mut tokens[i]),
                })
                .collect();
            hunks.push(Hunk { header: h.header.clone(), lines });
        }
        out.push(File { path: f.path.clone(), adds, dels, hunks });
    }

    Prepared { files: out, intraline: intraline_time, syntax: syntax_time }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_unified_diff;
    use crate::syntax::{Highlighters, Kind};

    const SAMPLE: &str = "\
diff --git a/a.rs b/a.rs
index 1111111..2222222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1; // old
+    let y = 1; // old
 }
";

    #[test]
    fn one_pass_produces_text_spans_and_tokens_together() {
        let hl = Highlighters::builtin();
        let p = prepare(&parse_unified_diff(SAMPLE), &hl, 2000);
        assert_eq!(p.files.len(), 1);
        let f = &p.files[0];
        assert_eq!((f.adds, f.dels), (1, 1));
        let lines = &f.hunks[0].lines;
        assert_eq!(lines.len(), 4);

        // The intraline pass found the one changed word...
        let removed = lines.iter().find(|l| l.kind == LineKind::Removed).unwrap();
        assert_eq!(&removed.text[removed.spans[0].start..removed.spans[0].end], "x");
        // ...and the syntax pass ran on the same text.
        assert!(removed.tokens.iter().any(|t| t.kind == Kind::Keyword));
        assert!(removed.tokens.iter().any(|t| t.kind == Kind::Comment));
    }

    #[test]
    fn every_range_indexes_the_line_it_belongs_to() {
        // The invariant the renderer depends on: nothing points past the text.
        let hl = Highlighters::builtin();
        let p = prepare(&parse_unified_diff(SAMPLE), &hl, 2000);
        for l in p.files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines) {
            for s in &l.spans {
                assert!(s.end <= l.text.len(), "span {s:?} outside {:?}", l.text);
            }
            for t in &l.tokens {
                assert!(t.end <= l.text.len(), "token {t:?} outside {:?}", l.text);
                assert!(l.text.is_char_boundary(t.start) && l.text.is_char_boundary(t.end));
            }
        }
    }

    #[test]
    fn clipping_happens_before_the_passes_not_after() {
        // A 4000-character line clipped to 40: no span and no token may describe
        // a byte the renderer will never draw.
        let long = "x".repeat(2000);
        let raw = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-let a = \"{long}\";\n+let b = \"{long}\";\n"
        );
        let hl = Highlighters::builtin();
        let p = prepare(&parse_unified_diff(&raw), &hl, 40);
        for l in p.files.iter().flat_map(|f| &f.hunks).flat_map(|h| &h.lines) {
            assert!(l.text.chars().count() < 80, "{:?}", l.text);
            assert!(l.spans.iter().all(|s| s.end <= l.text.len()));
            assert!(l.tokens.iter().all(|t| t.end <= l.text.len()));
        }
    }

    #[test]
    fn a_routed_highlighter_reaches_the_prepared_rows() {
        // The host's routing is not something the frontend re-implements.
        let raw = "diff --git a/r.md b/r.md\n@@ -1,1 +1,1 @@\n-# old\n+# new\n";
        let hl = Highlighters::builtin();
        let p = prepare(&parse_unified_diff(raw), &hl, 2000);
        let first = &p.files[0].hunks[0].lines[0];
        assert_eq!(first.tokens[0].kind, Kind::Heading, "markdown routing was lost");
    }
}
