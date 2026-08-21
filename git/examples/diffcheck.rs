//! Checks the differs against git's own answer on a real repository.
//!
//!     cargo run -q -p plait-git --example diffcheck --release [REPO] [REVSPEC]
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
//! **Not hunk boundaries.** Git runs `--indent-heuristic` by default, which
//! slides a hunk to a more readable equivalent position without changing what it
//! says. Two diffs of identical length and different hunk offsets are both
//! correct, so comparing offsets would be measuring a preference and reporting
//! it as a bug.

use plait_core::differ::Differs;
use plait_core::{parse_unified_diff, LineKind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let revspec = args.next().unwrap_or_default();

    let t = Instant::now();
    let pairs = match plait_git::pairs(&repo, &revspec) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let acquire = t.elapsed();
    let (old_lines, new_lines): (usize, usize) =
        pairs.iter().fold((0, 0), |(o, n), p| (o + p.old.len(), n + p.new.len()));

    println!(
        "{} {}\n  {} files  {} old lines  {} new lines  acquire {:.1?}  ({} binary)",
        repo.display(),
        if revspec.is_empty() { "(working tree)" } else { &revspec },
        pairs.len(),
        old_lines,
        new_lines,
        acquire,
        pairs.iter().filter(|p| p.binary).count(),
    );

    let mut mismatches = 0;
    for (name, flag) in
        [("histogram", "--histogram"), ("patience", "--patience"), ("myers", "--minimal")]
    {
        let mut differs = Differs::builtin();
        assert!(differs.select(name), "{name} is not registered");

        let t = Instant::now();
        let (mut adds, mut dels, mut hunks) = (0usize, 0usize, 0usize);
        for p in pairs.iter().filter(|p| !p.binary) {
            let old: Vec<&str> = p.old.iter().map(String::as_str).collect();
            let new: Vec<&str> = p.new.iter().map(String::as_str).collect();
            let f = differs.file(&p.path, &old, &new);
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

        let (g_adds, g_dels, g_hunks, g_time) = git_diff(&repo, &revspec, flag);
        report_worst(&repo, &revspec, flag, &differs, &pairs);
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
            "  {name:<10} +{adds:<7} -{dels:<7} {hunks:>6}h {ours:>9.1?}  │  \
             git {flag:<12} +{g_adds:<7} -{g_dels:<7} {g_hunks:>6}h {g_time:>9.1?}  {verdict}"
        );
    }

    println!(
        "\n{}",
        if mismatches == 0 {
            "every algorithm agrees with git on how many lines changed"
        } else {
            "a count outside tolerance means a changed answer, not a preference"
        }
    );
}

/// git's own answer, and what it cost.
fn git_diff(repo: &Path, revspec: &str, flag: &str) -> (usize, usize, usize, Duration) {
    let dir = repo.to_str().unwrap_or(".");
    let args: Vec<&str> = if revspec.is_empty() {
        vec!["-C", dir, "diff", "--no-ext-diff", flag, "-M", "HEAD"]
    } else if revspec.contains("..") {
        vec!["-C", dir, "diff", "--no-ext-diff", flag, "-M", revspec]
    } else {
        vec!["-C", dir, "show", "--no-ext-diff", flag, "-M", "--format=", revspec]
    };
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
    (adds, dels, hunks, elapsed)
}


/// The files this algorithm did worst on relative to git, so a regression has a
/// name and a path rather than only a total.
fn report_worst(
    repo: &Path,
    revspec: &str,
    flag: &str,
    differs: &Differs,
    pairs: &[plait_git::Pair],
) {
    if std::env::var_os("WORST").is_none() {
        return;
    }
    let dir = repo.to_str().unwrap_or(".");
    let args: Vec<&str> = if revspec.is_empty() {
        vec!["-C", dir, "diff", "--no-ext-diff", flag, "-M", "HEAD"]
    } else if revspec.contains("..") {
        vec!["-C", dir, "diff", "--no-ext-diff", flag, "-M", revspec]
    } else {
        vec!["-C", dir, "show", "--no-ext-diff", flag, "-M", "--format=", revspec]
    };
    let out = Command::new("git").args(&args).output().expect("git");
    let theirs = parse_unified_diff(&String::from_utf8_lossy(&out.stdout));
    let count = |f: &plait_core::FileDiff| -> usize {
        f.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind != LineKind::Context)
            .count()
    };
    let mut rows: Vec<(isize, String, usize, usize)> = Vec::new();
    for p in pairs.iter().filter(|p| !p.binary) {
        let old: Vec<&str> = p.old.iter().map(String::as_str).collect();
        let new: Vec<&str> = p.new.iter().map(String::as_str).collect();
        let ours = count(&differs.file(&p.path, &old, &new));
        let theirs = theirs
            .iter()
            .find(|f| f.path == p.path || Some(f.path.as_str()) == p.old_path.as_deref())
            .map(count)
            .unwrap_or(0);
        if ours != theirs {
            rows.push((ours as isize - theirs as isize, p.path.clone(), ours, theirs));
        }
    }
    rows.sort_by_key(|r| -r.0.abs());
    for (delta, path, ours, theirs) in rows.iter().take(6) {
        println!("      {delta:+6}  {path}  (ours {ours}, git {theirs})");
    }
}
