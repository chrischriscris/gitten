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

/// One file: clip, intraline, highlight. The whole of what [`prepare`] does,
/// factored out because it is also the unit of work handed to a worker — and
/// because a file is genuinely independent of every other file, which is the
/// property the parallelism rests on and the reason it needs no locking.
fn one_file(
    f: &FileDiff,
    hl: &dyn Highlighter,
    max_line_chars: usize,
) -> (File, Duration, Duration) {
    let mut intraline_time = Duration::ZERO;
    let mut syntax_time = Duration::ZERO;

    let all = || f.hunks.iter().flat_map(|h| &h.lines);
    let adds = all().filter(|l| l.kind == LineKind::Added).count();
    let dels = all().filter(|l| l.kind == LineKind::Removed).count();
    let mut hunks = Vec::with_capacity(f.hunks.len());

    for h in &f.hunks {
        let mut texts: Vec<Arc<str>> = h
            .lines
            .iter()
            .map(|l| clip(&l.text, max_line_chars))
            .collect();

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
        let refs: Vec<&str> = texts.iter().map(|t| &**t).collect();
        let kinds: Vec<LineKind> = h.lines.iter().map(|l| l.kind).collect();
        let mut tokens = highlight_hunk(hl, &f.path, &refs, &kinds);
        syntax_time += t.elapsed();

        let lines = h
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| Line {
                kind: l.kind,
                moved: l.moved,
                old_no: l.old_no,
                new_no: l.new_no,
                text: std::mem::take(&mut texts[i]),
                spans: std::mem::take(&mut spans[i]).into_boxed_slice(),
                tokens: std::mem::take(&mut tokens[i]).into_boxed_slice(),
            })
            .collect();
        hunks.push(Hunk {
            header: h.header.clone(),
            lines,
        });
    }
    let file = File {
        path: f.path.clone(),
        adds,
        dels,
        hunks,
    };
    (file, intraline_time, syntax_time)
}

/// A diff into rows, one file at a time — on as many cores as there are.
///
/// # Why this is where the threads are
///
/// A file is independent of every other file: nothing in [`one_file`] reads
/// another file's output, and the two expensive passes are pure functions of one
/// hunk's text. So this was one core doing 179 ms of work that ten cores can do
/// in 67, and it was the largest single thing between a keystroke and a redrawn
/// diff.
///
/// Threads in `core` are not a boundary violation — the rule is that `core` may
/// not know a UI exists, and a thread is arithmetic, not I/O. Putting them here
/// rather than in each client is the same argument as everything else in this
/// module: three frontends need the identical answer, and one that had to write
/// its own fan-out is one that would get the ordering wrong.
///
/// # Work stealing, not chunks
///
/// Workers pull the next file off a shared counter rather than taking a
/// contiguous slice each. Files are wildly uneven — `md.diff` is 229 files and
/// one of them is most of the work — so static chunks leave nine cores idle
/// while one finishes, and measured 2.1× where stealing measures better. It is
/// four lines of `AtomicUsize` either way.
///
/// # The output is order-for-order identical to doing it serially
///
/// Rows address files by index and a client caches by it, so a reordered
/// `files` is not a cosmetic difference — it is every row pointing at the wrong
/// file. Workers therefore carry each result's index with it and the whole lot
/// is sorted back before it is returned, which costs one sort of a few thousand
/// small items and buys a guarantee. `parallel_and_serial_agree_exactly` is the
/// test, and it compares the `Vec<File>`s whole rather than sampling.
///
/// **A single-file diff gets nothing from this.** The unit of work is a file, so
/// one 700k-line file is still one core. Stealing hunks instead would fix that
/// and is the next thing here; it needs the per-file timing accumulation to move,
/// which is why it is not in this pass.
pub fn prepare(files: &[FileDiff], hl: &dyn Highlighter, max_line_chars: usize) -> Prepared {
    let lines: usize = files
        .iter()
        .flat_map(|f| &f.hunks)
        .map(|h| h.lines.len())
        .sum();
    let workers = match lines > PARALLEL_ABOVE && files.len() > 1 {
        true => std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(files.len()),
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

    let next = AtomicUsize::new(0);
    let batches: Vec<Vec<(usize, File, Duration, Duration)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(f) = files.get(i) else { break };
                        let (file, intra, syn) = one_file(f, hl, max_line_chars);
                        mine.push((i, file, intra, syn));
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

    let mut done: Vec<(usize, File, Duration, Duration)> = batches.into_iter().flatten().collect();
    done.sort_unstable_by_key(|(i, ..)| *i);
    let (mut intra, mut syn) = (Duration::ZERO, Duration::ZERO);
    let mut out = Vec::with_capacity(done.len());
    for (_, file, i, s) in done {
        out.push(file);
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
}
