//! Checks the differs against git's own answer on a real repository.
//!
//!     cargo run -q -p gitten-git --example diffcheck --release [REPO] [REVSPEC]
//!
//! `core`'s tests prove an edit script applies and that Myers is minimal, on
//! inputs small enough to check by hand. This is the other half: real files,
//! real encodings, real renames, against the tool everybody compares to.
//!
//! # What is compared, and what deliberately is not
//!
//! **Changed-line counts, per algorithm.** `git diff --histogram` against our
//! Histogram, `--minimal` against our Myers. A minimal script has exactly one
//! length, so **Myers must match exactly**; if it does not, one of the two is
//! not minimal and that is a bug.
//!
//! The anchored two are held to a looser standard, because "best anchor" is a
//! judgement and not a quantity. Ours and git's histogram agree exactly on every
//! repository checked here; our patience is patience's *idea* through the
//! histogram machinery rather than git's longest-increasing-subsequence
//! implementation, and diverges by a fraction of a percent. A drift past 1% is
//! flagged — that is a changed answer, not a preference.
//!
//! **Hunk positions, exactly.** Every `@@ -a,b +c,d @@` we emit is compared
//! against git's for the same file. This used to be skipped on the grounds that
//! git's `--indent-heuristic` slides hunks and a different offset is still a
//! correct diff — true before the heuristic was ported, and it hid a real bug:
//! the port scored a position's indentation by magnitude where git compares it by
//! sign, which slid hunks to plausible-looking places git does not put them. The
//! line counts were identical throughout. Compare the positions.
//!
//! The function-name suffix is not compared: ours stops looking 400 lines above
//! the hunk and git's does not, which is a bounded difference in a string nobody
//! diffs.

use gitten_core::differ::{Differs, Overrides, Whitespace};
use gitten_core::{parse_unified_diff, LineKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let revspec = args.next().unwrap_or_default();

    let t = Instant::now();
    let pairs = match gitten_git::pairs(&repo, &revspec) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let acquire = t.elapsed();
    let (old_lines, new_lines): (usize, usize) = pairs
        .iter()
        .fold((0, 0), |(o, n), p| (o + p.old.len(), n + p.new.len()));

    println!(
        "{} {}\n  {} files  {} old lines  {} new lines  acquire {:.1?}  ({} binary)",
        repo.display(),
        if revspec.is_empty() {
            "(working tree)"
        } else {
            &revspec
        },
        pairs.len(),
        old_lines,
        new_lines,
        acquire,
        pairs.iter().filter(|p| p.binary).count(),
    );

    let mut mismatches = 0;
    // The whitespace rows keep `--histogram` on git's side as well: a whitespace
    // relation is a property of how lines are compared, not of the algorithm, so
    // comparing ours-on-histogram against git's-on-myers measures the wrong
    // thing. That mistake cost a real half hour — it reported a two-line
    // difference on this file and the cause was the flag list, not the code.
    for (name, algorithm, flags, ws) in [
        (
            "histogram",
            "histogram",
            &["--histogram"][..],
            Whitespace::Exact,
        ),
        ("patience", "patience", &["--patience"], Whitespace::Exact),
        ("myers", "myers", &["--minimal"], Whitespace::Exact),
        (
            "ws-eol",
            "histogram",
            &["--histogram", "--ignore-space-at-eol"],
            Whitespace::Trailing,
        ),
        (
            "ws-change",
            "histogram",
            &["--histogram", "-b"],
            Whitespace::Change,
        ),
        (
            "ws-all",
            "histogram",
            &["--histogram", "-w"],
            Whitespace::All,
        ),
    ] {
        let mut differs = Differs::builtin();
        assert!(differs.select(algorithm), "{algorithm} is not registered");
        differs.whitespace = ws;
        // Off for the comparison: git does not report moves in its line counts
        // either, and a move is a presentation of the same script.
        differs.min_moved = 0;

        let t = Instant::now();
        let (mut adds, mut dels, mut hunks) = (0usize, 0usize, 0usize);
        let mut ours_ranges: Vec<(String, Vec<String>)> = Vec::new();
        for p in pairs.iter().filter(|p| !p.binary) {
            let f = differs.file_using(&Overrides::default(), &p.path, &p.old, &p.new);
            ours_ranges.push((p.path.clone(), ranges(&f)));
            hunks += f.hunks.len();
            for l in f.hunks.iter().flat_map(|h| &h.lines) {
                match l.kind {
                    LineKind::Added => adds += 1,
                    LineKind::Removed => dels += 1,
                    LineKind::Context => {}
                }
            }
        }
        let ours = t.elapsed();

        let (g_adds, g_dels, g_hunks, g_time, theirs) = git_diff(&repo, &revspec, flags);
        report_worst(&repo, &revspec, flags, &differs, &pairs);

        // Where the hunks are, not just how many. `ranges` drops the function
        // suffix and keeps `@@ -a,b +c,d`.
        let mut misplaced = 0usize;
        for (path, ours) in &ours_ranges {
            let g = theirs
                .iter()
                .find(|f| &f.path == path)
                .map(ranges)
                .unwrap_or_default();
            misplaced += ours.iter().zip(g.iter()).filter(|(a, b)| a != b).count()
                + ours.len().abs_diff(g.len());
        }
        // Positions must match — except for myers, where they need not. A
        // minimal script has one *length* and not one *shape*: several scripts
        // of that length exist and ours picks a different one from git's, which
        // the slide then places differently. The line counts agreeing is what
        // proves both are still minimal. The anchored rows have no such freedom
        // and are held to exact positions, which is what verifies `compact`.
        let hunk_note = match (misplaced, name) {
            (0, _) => String::new(),
            (n, "myers") => format!(" · {n}/{hunks} placed differently (both minimal)"),
            (n, _) => {
                mismatches += 1;
                format!(" · {n}/{hunks} hunks IN THE WRONG PLACE")
            }
        };
        // Myers has one correct length; the anchored ones have a range of
        // defensible ones, so only a drift past a fraction of a percent means
        // anything.
        let drift = (adds + dels) as isize - (g_adds + g_dels) as isize;
        let tolerance = match name {
            "myers" => 0,
            _ => (g_adds + g_dels) as isize / 100,
        };
        let verdict = if drift == 0 {
            "=".to_string()
        } else if drift.abs() <= tolerance {
            format!("{drift:+} of {} — within tolerance", g_adds + g_dels)
        } else {
            mismatches += 1;
            format!("{drift:+} of {} — TOO FAR", g_adds + g_dels)
        };
        println!(
            "  {name:<10} +{adds:<7} -{dels:<7} {hunks:>6}h {ours:>9.1?}  │  git {:<28} \
             +{g_adds:<7} -{g_dels:<7} {g_hunks:>6}h {g_time:>9.1?}  {verdict}{hunk_note}",
            flags.join(" "),
        );
    }

    println!(
        "\n{}",
        if mismatches == 0 {
            "every algorithm agrees with git on how many lines changed"
        } else {
            "a count or a position outside tolerance means a changed answer"
        }
    );
}

/// git's own answer, and what it cost.
/// A hunk header without its function-name suffix: `@@ -41,9 +41,11 @@`.
///
/// `core`'s split, not a second copy of it: this had its own two-line version of
/// the same scan, which is one place for the two to disagree about what a header
/// is — and this comparison is the thing that decides whether a differ is right.
fn ranges(f: &gitten_core::FileDiff) -> Vec<String> {
    f.hunks
        .iter()
        .map(|h| gitten_core::hunk_parts(&h.header).0.to_string())
        .collect()
}

fn git_diff(
    repo: &Path,
    revspec: &str,
    flags: &[&str],
) -> (usize, usize, usize, Duration, Vec<gitten_core::FileDiff>) {
    let dir = repo.to_str().unwrap_or(".");
    let mut args: Vec<&str> = if revspec.is_empty() {
        vec!["-C", dir, "diff", "--no-ext-diff", "-M", "HEAD"]
    } else if revspec.contains("..") {
        vec!["-C", dir, "diff", "--no-ext-diff", "-M", revspec]
    } else {
        // Ask git what pairs() asks: --diff-merges must match the acquisition
        // layer's show path or a merge answers empty here.
        vec![
            "-C",
            dir,
            "show",
            "--no-ext-diff",
            "-M",
            "--format=",
            "--diff-merges=first-parent",
            revspec,
        ]
    };
    args.extend_from_slice(flags);
    let t = Instant::now();
    let out = Command::new("git").args(&args).output().expect("git");
    let elapsed = t.elapsed();
    let files = parse_unified_diff(&String::from_utf8_lossy(&out.stdout));
    let (mut adds, mut dels, mut hunks) = (0, 0, 0);
    for f in &files {
        hunks += f.hunks.len();
        for l in f.hunks.iter().flat_map(|h| &h.lines) {
            match l.kind {
                LineKind::Added => adds += 1,
                LineKind::Removed => dels += 1,
                LineKind::Context => {}
            }
        }
    }
    (adds, dels, hunks, elapsed, files)
}

/// The files this algorithm did worst on relative to git, so a regression has a
/// name and a path rather than only a total.
fn report_worst(
    repo: &Path,
    revspec: &str,
    flags: &[&str],
    differs: &Differs,
    pairs: &[gitten_git::Pair],
) {
    if std::env::var_os("WORST").is_none() {
        return;
    }
    let dir = repo.to_str().unwrap_or(".");
    let mut args: Vec<&str> = if revspec.is_empty() {
        vec!["-C", dir, "diff", "--no-ext-diff", "-M", "HEAD"]
    } else if revspec.contains("..") {
        vec!["-C", dir, "diff", "--no-ext-diff", "-M", revspec]
    } else {
        // Same oracle as git_diff: --diff-merges must match pairs()'s show path.
        vec![
            "-C",
            dir,
            "show",
            "--no-ext-diff",
            "-M",
            "--format=",
            "--diff-merges=first-parent",
            revspec,
        ]
    };
    args.extend_from_slice(flags);
    let out = Command::new("git").args(&args).output().expect("git");
    let theirs = parse_unified_diff(&String::from_utf8_lossy(&out.stdout));
    let count = |f: &gitten_core::FileDiff| -> usize {
        f.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind != LineKind::Context)
            .count()
    };
    let mut rows: Vec<(isize, String, usize, usize)> = Vec::new();
    for p in pairs.iter().filter(|p| !p.binary) {
        let ours = count(&differs.file_using(&Overrides::default(), &p.path, &p.old, &p.new));
        let theirs = theirs
            .iter()
            .find(|f| f.path == p.path || Some(f.path.as_str()) == p.old_path.as_deref())
            .map(count)
            .unwrap_or(0);
        if ours != theirs {
            rows.push((
                ours as isize - theirs as isize,
                p.path.clone(),
                ours,
                theirs,
            ));
        }
    }
    rows.sort_by_key(|r| -r.0.abs());
    for (delta, path, ours, theirs) in rows.iter().take(6) {
        println!("      {delta:+6}  {path}  (ours {ours}, git {theirs})");
    }
}
