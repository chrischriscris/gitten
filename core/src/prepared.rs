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
use crate::{intraline_with, replace_pairs, FileDiff, LineKind, Span};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One line, with everything a renderer needs and nothing it would have to
/// recompute: the text as it will be shown, the words that changed inside it,
/// and its syntax tokens. Ranges index `text`, so they can never point past
/// what is on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// Part of a block that moved rather than changed. Carried through from
    /// [`crate::DiffLine`] untouched — the passes in here have nothing to say
    /// about it, and the renderer needs it.
    pub moved: bool,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// The same allocation the parsed [`crate::DiffLine`] holds whenever the
    /// line fit the clip budget: `clip`'s fast path is a refcount bump. A
    /// frontend's rows take another bump rather than a copy.
    pub text: Arc<str>,
    /// Never mutated after `prepare` — hence exact-size boxed slices rather
    /// than `Vec`s with spare capacity. Markdown layout rebuilds these rather
    /// than editing them in place.
    pub spans: Box<[Span]>,
    pub tokens: Box<[Token]>,
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
    /// Spans or tokens [`sanitize`] threw away because they pointed mid-character,
    /// past the end of their line, or out of order. Zero for every built-in pass;
    /// see [`Prepared::rejected`] for why this exists at all.
    pub rejected: usize,
}

/// The prepared diff, plus what the two expensive passes cost. The timings are
/// here because every frontend wants to report them and none of them should be
/// timing this themselves.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub files: Vec<File>,
    /// **CPU time, summed across workers — not wall clock.** [`prepare`] runs
    /// files in parallel, so this is what the pass cost and not how long you
    /// waited for it; on a ten-core machine the two differ by most of an order
    /// of magnitude, and `intraline + syntax` no longer adds up to the duration
    /// of the `prepare` call that produced them.
    ///
    /// Summed rather than wall-clocked on purpose: it is the number that is
    /// comparable between runs and between fixtures, which is what every caller
    /// reports it for. Wall clock is the caller's own `Instant` around the call,
    /// and [`Prepared::threads`] is what says how far apart the two should be.
    pub intraline: Duration,
    /// CPU time, summed across workers. See [`Prepared::intraline`].
    pub syntax: Duration,
    /// How many workers ran, so a reader can tell a slow pass from a wide one.
    /// 1 when the diff was small enough to do in place.
    pub threads: usize,
}

impl Prepared {
    pub fn lines(&self) -> usize {
        self.files
            .iter()
            .flat_map(|f| &f.hunks)
            .map(|h| h.lines.len())
            .sum()
    }

    /// How many spans or tokens [`sanitize`] threw away. Zero for everything
    /// shipped; a frontend reports it so a `Differ` or `Highlighter` extension
    /// that is quietly handing back bad ranges says so, the same way
    /// [`crate::wrap::Wrapped::rejected`] does for a bad `Wrap`.
    pub fn rejected(&self) -> usize {
        self.files.iter().map(|f| f.rejected).sum()
    }
}

/// Keeps the spans or tokens that describe a range of `text` and drops the
/// rest, counting what it drops.
///
/// Not defensiveness for its own sake: a range that points past its line, or
/// into the middle of a character, is a slice panic on the render path —
/// `runs_selected` folds these edges in with `.min` only, and it never sees
/// the text to check them against. `Highlighter` is an extension seam and
/// `intraline` is not exempt either, so this runs on both. The rules mirror
/// [`crate::wrap::Wrapped::take`]: in range, on character boundaries, and
/// ascending — `runs` relies on ordered input to terminate.
///
/// `retain` rather than a rebuilt `Vec`: nearly every span and token a real
/// pass produces is valid, so this costs a scan and not an allocation.
fn sanitize<T>(
    text: &str,
    items: &mut Vec<T>,
    range: impl Fn(&T) -> (u32, u32),
    rejected: &mut usize,
) {
    let mut prev_end = 0u32;
    items.retain(|item| {
        let (start, end) = range(item);
        let ok = start >= prev_end
            && end >= start
            && (end as usize) <= text.len()
            && text.is_char_boundary(start as usize)
            && text.is_char_boundary(end as usize);
        if ok {
            prev_end = end;
        } else {
            *rejected += 1;
        }
        ok
    });
}

/// Real repos contain minified bundles and base64 blobs; a single line of 9.6
/// million characters was measured in the wild. Text layout is linear in length,
/// so one such line stalls a frame. Nobody reads past column 2000 either way.
///
/// Clipping happens *before* both passes, so their output can only ever describe
/// text that will actually be drawn.
pub fn clip(s: &Arc<str>, max_chars: usize) -> Arc<str> {
    // A character is one to four bytes, so a line that fits the budget in
    // bytes fits it in characters too — and the count below is a full decode
    // walk that nearly every line of a real diff should never pay. The fast
    // path shares the caller's allocation; this is where parse copy A and
    // prepared copy B become one.
    if s.len() <= max_chars {
        return s.clone();
    }
    // Past the cheap check the count is still the arbiter, because byte
    // length overshoots for multibyte text. One walk produces both the total
    // and where the head ends, so the head is sliced rather than collected.
    let mut n = 0;
    let mut head_end = None;
    for (i, _) in s.char_indices() {
        if n == max_chars {
            head_end = Some(i);
        }
        n += 1;
    }
    let head_end = match head_end {
        Some(i) => i,
        None => return s.clone(),
    };
    Arc::from(format!(
        "{}  … {} more chars",
        &s[..head_end],
        n - max_chars
    ))
}

/// Below this many lines, do it in place.
///
/// Spawning a handful of threads and joining them is tens of microseconds, which
/// is worth it against a diff that takes milliseconds and pure loss against one
/// that takes less. Lines and not *files*, because file count says nothing about
/// the work: `md.diff` is 229 files and 94k lines, and one of those files is most
/// of it.
const PARALLEL_ABOVE: usize = 2_000;

/// One hunk: clip, intraline, highlight, sanitize. The unit of work a worker
/// pulls, and independent of every other hunk — nothing in here reads outside
/// the hunk except the file's path, which only picks a lexer.
fn one_hunk(
    h: &crate::Hunk,
    path: &str,
    hl: &dyn Highlighter,
    max_line_chars: usize,
    lcs: &mut Vec<u32>,
) -> (Hunk, Duration, Duration, usize) {
    let mut texts: Vec<Arc<str>> = h
        .lines
        .iter()
        .map(|l| clip(&l.text, max_line_chars))
        .collect();

    // Second pass: only the removed/added pairs a line diff already matched get
    // word-level spans. The table allocation survives every pair this worker
    // handles.
    let mut spans: Vec<Vec<Span>> = vec![Vec::new(); h.lines.len()];
    let t = Instant::now();
    for (d, a) in replace_pairs(h) {
        let (o, n) = intraline_with(&texts[d], &texts[a], lcs);
        spans[d] = o;
        spans[a] = n;
    }
    let intraline_time = t.elapsed();

    let t = Instant::now();
    let refs: Vec<&str> = texts.iter().map(|t| &**t).collect();
    let kinds: Vec<LineKind> = h.lines.iter().map(|l| l.kind).collect();
    let mut tokens = highlight_hunk(hl, path, &refs, &kinds);
    let syntax_time = t.elapsed();

    let mut rejected = 0;
    for i in 0..h.lines.len() {
        sanitize(
            &texts[i],
            &mut spans[i],
            |s| (s.start, s.end),
            &mut rejected,
        );
        sanitize(
            &texts[i],
            &mut tokens[i],
            |t| (t.start, t.end),
            &mut rejected,
        );
    }

    let mut lines = Vec::with_capacity(h.lines.len());
    for (i, l) in h.lines.iter().enumerate() {
        lines.push(Line {
            kind: l.kind,
            moved: l.moved,
            old_no: l.old_no,
            new_no: l.new_no,
            text: std::mem::take(&mut texts[i]),
            tokens: std::mem::take(&mut tokens[i]).into_boxed_slice(),
            spans: std::mem::take(&mut spans[i]).into_boxed_slice(),
        });
    }
    (
        Hunk {
            header: h.header.clone(),
            lines,
        },
        intraline_time,
        syntax_time,
        rejected,
    )
}

/// One file through the same hunk-shaped work used by the parallel path.
fn one_file(
    f: &FileDiff,
    hl: &dyn Highlighter,
    max_line_chars: usize,
) -> (File, Duration, Duration) {
    let all = || f.hunks.iter().flat_map(|h| &h.lines);
    let adds = all().filter(|l| l.kind == LineKind::Added).count();
    let dels = all().filter(|l| l.kind == LineKind::Removed).count();
    let mut hunks = Vec::with_capacity(f.hunks.len());
    let (mut intraline_time, mut syntax_time) = (Duration::ZERO, Duration::ZERO);
    let mut rejected = 0;
    let mut lcs = Vec::new();
    for h in &f.hunks {
        let (hunk, intra, syntax, bad) = one_hunk(h, &f.path, hl, max_line_chars, &mut lcs);
        hunks.push(hunk);
        intraline_time += intra;
        syntax_time += syntax;
        rejected += bad;
    }
    (
        File {
            path: f.path.clone(),
            adds,
            dels,
            hunks,
            rejected,
        },
        intraline_time,
        syntax_time,
    )
}

/// A diff into rows, one hunk at a time — on as many cores as there are.
///
/// # Why this is where the threads are
///
/// A hunk is independent of every other hunk: nothing in [`one_hunk`] reads a
/// neighbour's output, and the two expensive passes are pure functions of its
/// text. Threads in `core` are arithmetic, not I/O, and keep every frontend on
/// the same ordering and timing semantics.
///
/// # Work stealing, not chunks
///
/// Workers pull the next hunk off a shared counter rather than taking a
/// contiguous slice each. Hunks are wildly uneven, so static chunks leave cores
/// idle. The line floor still decides whether spawning is worthwhile.
///
/// # The output is order-for-order identical to doing it serially
///
/// Rows address files and hunks by index. Workers therefore carry both indices,
/// results are sorted back into input order, and files with no hunks are built
/// from the input rather than disappearing from a hunk-shaped work list.
///
/// A diff of one hunk is the remaining serial floor. Splitting below it would
/// split a syntax-highlighting run, which may carry state across its lines.
pub fn prepare(files: &[FileDiff], hl: &dyn Highlighter, max_line_chars: usize) -> Prepared {
    let lines: usize = files
        .iter()
        .flat_map(|f| &f.hunks)
        .map(|h| h.lines.len())
        .sum();
    let hunks: usize = files.iter().map(|f| f.hunks.len()).sum();
    // Hunks, not files: a large diff of one file is the case the file-shaped
    // gate excluded. Below the line floor, threading is pure loss regardless of
    // shape.
    let workers = match lines > PARALLEL_ABOVE && hunks > 1 {
        true => std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(hunks),
        false => 1,
    };

    if workers <= 1 {
        let mut out = Vec::with_capacity(files.len());
        let (mut intra, mut syn) = (Duration::ZERO, Duration::ZERO);
        for f in files {
            let (file, i, s) = one_file(f, hl, max_line_chars);
            out.push(file);
            intra += i;
            syn += s;
        }
        return Prepared {
            files: out,
            intraline: intra,
            syntax: syn,
            threads: 1,
        };
    }

    // One flat list of (file, hunk), so a worker's counter is an index and not a
    // search. Built once, outside the threads.
    let work: Vec<(u32, u32)> = files
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| (0..f.hunks.len()).map(move |hi| (fi as u32, hi as u32)))
        .collect();
    let next = AtomicUsize::new(0);
    type Done = (u32, u32, Hunk, Duration, Duration, usize);
    let batches: Vec<Vec<Done>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    let mut lcs = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(fi, hi)) = work.get(i) else {
                            break;
                        };
                        let file = &files[fi as usize];
                        let (hunk, intra, syn, rejected) = one_hunk(
                            &file.hunks[hi as usize],
                            &file.path,
                            hl,
                            max_line_chars,
                            &mut lcs,
                        );
                        mine.push((fi, hi, hunk, intra, syn, rejected));
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            // Resumed rather than swallowed, exactly as when this was one loop
            // on the caller's thread: a panic in the highlighter is a bug to see,
            // not a diff to silently truncate.
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });

    let mut done: Vec<Done> = batches.into_iter().flatten().collect();
    done.sort_unstable_by_key(|(fi, hi, ..)| (*fi, *hi));
    let mut out: Vec<File> = files
        .iter()
        .map(|f| {
            let all = || f.hunks.iter().flat_map(|h| &h.lines);
            File {
                path: f.path.clone(),
                adds: all().filter(|l| l.kind == LineKind::Added).count(),
                dels: all().filter(|l| l.kind == LineKind::Removed).count(),
                hunks: Vec::with_capacity(f.hunks.len()),
                rejected: 0,
            }
        })
        .collect();
    let (mut intra, mut syn) = (Duration::ZERO, Duration::ZERO);
    for (fi, _, hunk, i, s, rejected) in done {
        let file = &mut out[fi as usize];
        file.hunks.push(hunk);
        file.rejected += rejected;
        intra += i;
        syn += s;
    }

    Prepared {
        files: out,
        intraline: intra,
        syntax: syn,
        threads: workers,
    }
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

    /// A diff big enough to cross [`PARALLEL_ABOVE`], across enough files to
    /// give the workers something to steal, with sizes deliberately uneven so
    /// they finish out of order.
    fn wide_diff() -> String {
        let mut raw = String::new();
        for f in 0..40 {
            raw.push_str(&format!("diff --git a/f{f}.rs b/f{f}.rs\n"));
            // 1 to 40 hunks: the last file is forty times the first, so whoever
            // takes it finishes long after everyone else.
            for h in 0..=f {
                raw.push_str(&format!("@@ -{},3 +{},3 @@\n", h * 10 + 1, h * 10 + 1));
                raw.push_str(&format!(" fn keep{h}() {{}}\n"));
                raw.push_str(&format!("-    let was = {h}; // old\n"));
                raw.push_str(&format!("+    let now = {h}; // old\n"));
            }
        }
        raw
    }

    fn one_file_many_hunks() -> String {
        let mut raw = String::from("diff --git a/one.rs b/one.rs\n");
        for h in 0..1001 {
            raw.push_str(&format!("@@ -{},2 +{},2 @@\n", h * 10 + 1, h * 10 + 1));
            raw.push_str(&format!(" fn keep{h}() {{}}\n"));
            raw.push_str(&format!("-let old{h} = 1;\n"));
            raw.push_str(&format!("+let new{h} = 1;\n"));
        }
        raw
    }

    #[test]
    fn parallel_and_serial_agree_exactly() {
        // The property the whole fan-out rests on. Rows address files by index
        // and a client caches by it, so a reordered `files` is not cosmetic —
        // it is every row pointing at the wrong file. Compared whole rather
        // than sampled, because the interesting failure is one file in forty.
        let hl = Highlighters::builtin();
        let files = parse_unified_diff(&wide_diff());
        let parallel = prepare(&files, &hl, 2000);
        assert!(
            parallel.threads > 1,
            "the fixture did not reach the parallel path"
        );

        // The serial path, reached the way a real caller reaches it: one file at
        // a time, which is also what a diff under the threshold does.
        let mut serial = Vec::new();
        for f in &files {
            serial.push(one_file(f, &hl, 2000).0);
        }
        assert_eq!(parallel.files, serial);
    }

    #[test]
    fn one_file_of_many_hunks_agrees_with_serial() {
        let hl = Highlighters::builtin();
        let files = parse_unified_diff(&one_file_many_hunks());
        let parallel = prepare(&files, &hl, 2000);
        assert!(parallel.threads > 1);
        assert_eq!(parallel.files, vec![one_file(&files[0], &hl, 2000).0]);
    }

    #[test]
    fn a_file_with_no_hunks_survives() {
        let hl = Highlighters::builtin();
        let normal = parse_unified_diff(&one_file_many_hunks()).remove(0);
        let files = vec![
            normal.clone(),
            FileDiff {
                path: "empty.rs".into(),
                hunks: Vec::new(),
            },
            normal,
        ];
        let prepared = prepare(&files, &hl, 2000);
        assert!(prepared.threads > 1);
        assert_eq!(prepared.files.len(), 3);
        assert_eq!(prepared.files[1].path, "empty.rs");
        assert!(prepared.files[1].hunks.is_empty());
    }

    #[test]
    fn hunks_keep_their_order_within_a_file() {
        let hl = Highlighters::builtin();
        let files = parse_unified_diff(&one_file_many_hunks());
        let expected: Vec<_> = files[0].hunks.iter().map(|h| h.header.clone()).collect();
        let prepared = prepare(&files, &hl, 2000);
        let actual: Vec<_> = prepared.files[0]
            .hunks
            .iter()
            .map(|h| h.header.clone())
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn rejected_sums_across_hunks() {
        struct Bad;
        impl Highlighter for Bad {
            fn highlight(&self, _path: &str, lines: &[&str]) -> Vec<Vec<Token>> {
                lines
                    .iter()
                    .map(|line| {
                        line.contains("bad")
                            .then_some(vec![Token {
                                start: 0,
                                end: 99,
                                kind: Kind::Keyword,
                            }])
                            .unwrap_or_default()
                    })
                    .collect()
            }
        }

        let files = parse_unified_diff(
            "diff --git a/a.rs b/a.rs\n\
             @@ -1 +1 @@\n-old\n+bad one\n\
             @@ -10 +10 @@\n-old\n+bad two\n",
        );
        let prepared = prepare(&files, &Bad, 2000);
        assert_eq!(prepared.files[0].rejected, 2);
    }

    #[test]
    fn a_small_diff_is_not_worth_a_thread() {
        let hl = Highlighters::builtin();
        let p = prepare(&parse_unified_diff(SAMPLE), &hl, 2000);
        assert_eq!(p.threads, 1, "four lines spawned a thread pool");
    }

    #[test]
    fn the_timings_are_cpu_time_and_say_how_many_workers_earned_them() {
        // The one behavioural consequence a reader has to know about: these no
        // longer sum to the wall clock of the call. Assert the relationship
        // rather than a duration, so it holds on any machine.
        let hl = Highlighters::builtin();
        let files = parse_unified_diff(&wide_diff());
        let wall = Instant::now();
        let p = prepare(&files, &hl, 2000);
        let wall = wall.elapsed();
        assert!(p.threads > 1);
        assert!(
            p.intraline + p.syntax <= wall * p.threads as u32,
            "summed CPU exceeded what {} workers could have spent in {wall:?}",
            p.threads
        );
    }

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
        assert_eq!(
            &removed.text[removed.spans[0].start as usize..removed.spans[0].end as usize],
            "x"
        );
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
                assert!(
                    s.end as usize <= l.text.len(),
                    "span {s:?} outside {:?}",
                    l.text
                );
            }
            for t in &l.tokens {
                assert!(
                    t.end as usize <= l.text.len(),
                    "token {t:?} outside {:?}",
                    l.text
                );
                assert!(
                    l.text.is_char_boundary(t.start as usize)
                        && l.text.is_char_boundary(t.end as usize)
                );
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
            assert!(l.spans.iter().all(|s| s.end as usize <= l.text.len()));
            assert!(l.tokens.iter().all(|t| t.end as usize <= l.text.len()));
        }
    }

    #[test]
    fn a_routed_highlighter_reaches_the_prepared_rows() {
        // The host's routing is not something the frontend re-implements.
        let raw = "diff --git a/r.md b/r.md\n@@ -1,1 +1,1 @@\n-# old\n+# new\n";
        let hl = Highlighters::builtin();
        let p = prepare(&parse_unified_diff(raw), &hl, 2000);
        let first = &p.files[0].hunks[0].lines[0];
        assert_eq!(
            first.tokens[0].kind,
            Kind::Heading,
            "markdown routing was lost"
        );
    }

    fn arc(s: &str) -> Arc<str> {
        s.into()
    }

    #[test]
    fn clipping_is_counted_in_characters_not_bytes() {
        // The byte-length fast path must not change what comes out: multibyte
        // text can overshoot in bytes while fitting in characters, and the
        // suffix always names characters.
        let wide = Arc::<str>::from("\u{4e2d}".repeat(60)); // three bytes each, 180 bytes total
        assert_eq!(
            clip(&wide, 60),
            wide,
            "fits in characters despite the byte length"
        );
        assert_eq!(
            clip(&wide, 10).as_ref(),
            format!("{}  … {} more chars", &wide[..30], 50)
        );

        let ascii: Arc<str> = "x".repeat(100).into();
        assert_eq!(clip(&ascii, 100), ascii);
        assert_eq!(
            clip(&ascii, 99).as_ref(),
            format!("{}  … 1 more chars", &ascii[..99])
        );
        assert_eq!(clip(&arc(""), 10).as_ref(), "");
        assert_eq!(clip(&arc("éé"), 0).as_ref(), "  … 2 more chars");
    }

    /// The `Highlighter` trait is an extension seam, so `prepare` must survive
    /// one that hands back bad ranges — the whole point of [`sanitize`]. Two of
    /// three tokens are wrong on purpose: one ends inside the two-byte `é`, one
    /// overruns the line, and the third is what a real highlighter would have
    /// produced. Multi-byte text on purpose, because a boundary bug is invisible
    /// on ASCII.
    #[test]
    fn a_bad_highlighter_loses_only_the_bad_tokens() {
        struct BadHighlighter;
        impl Highlighter for BadHighlighter {
            fn highlight(&self, _path: &str, lines: &[&str]) -> Vec<Vec<Token>> {
                lines
                    .iter()
                    .map(|line| {
                        if line.contains("héllo") {
                            vec![
                                // Ends at byte 2, inside `é` (bytes 1..3).
                                Token {
                                    start: 0,
                                    end: 2,
                                    kind: Kind::Keyword,
                                },
                                // Past the end of the (clipped) line.
                                Token {
                                    start: 0,
                                    end: 99,
                                    kind: Kind::Keyword,
                                },
                                // "world" — the one a real highlighter would emit.
                                Token {
                                    start: 7,
                                    end: 12,
                                    kind: Kind::Keyword,
                                },
                            ]
                        } else {
                            Vec::new()
                        }
                    })
                    .collect()
            }
        }

        const MULTIBYTE: &str = "\
diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1 +1,2 @@
 hello
+héllo world
";
        let files = parse_unified_diff(MULTIBYTE);
        let p = prepare(&files, &BadHighlighter, 2000);
        assert_eq!(
            p.rejected(),
            2,
            "the mid-character end and the overrun should both be dropped"
        );

        let line = &p.files[0].hunks[0].lines[1];
        assert_eq!(line.text.as_ref(), "héllo world");
        assert_eq!(line.tokens.len(), 1, "only the valid token survives");
        assert_eq!(&line.text[line.tokens[0].range()], "world");
    }

    #[test]
    fn sanitize_drops_mid_character_overrun_and_out_of_order() {
        // "héllo": h=0, é=1..3 (two bytes), l=3, l=4, o=5, len=6. Byte 2 is
        // inside `é` and is not a boundary.
        let text = "héllo";
        let mut items: Vec<Span> = vec![
            Span { start: 0, end: 2 },  // mid-character end
            Span { start: 0, end: 99 }, // past the end of the text
            Span { start: 3, end: 4 },  // valid: "l"
            Span { start: 1, end: 3 },  // out of order: starts before the last kept end
            Span { start: 4, end: 6 },  // valid: "lo"
        ];
        let mut rejected = 0;
        sanitize(text, &mut items, |s| (s.start, s.end), &mut rejected);
        assert_eq!(
            rejected, 3,
            "mid-character, overrun and out-of-order are each one rejection"
        );
        assert_eq!(
            items,
            vec![Span { start: 3, end: 4 }, Span { start: 4, end: 6 }]
        );
    }
}
