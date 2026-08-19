//! Getting data out of a real repository.
//!
//! `core` is pure and does no I/O; this crate is the layer that actually talks
//! to git. Today it shells out to the `git` binary for everything. Reads will
//! move to `gix` later for speed — see AGENTS.md — but writes stay here
//! permanently, because shelling out is what gets hooks, credential helpers and
//! `.gitconfig` semantics exactly right.

use plait_core::{parse_log, parse_unified_diff, Commit, FileDiff};
use std::path::Path;
use std::process::Command;

pub type Result<T> = std::result::Result<T, String>;

/// Must match `plait_core::parse_log`.
const LOG_FORMAT: &str = "%H%x1f%h%x1f%P%x1f%an%x1f%at%x1f%s%x1e";

fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args.join(" "), err.trim()));
    }
    Ok(out.stdout)
}

/// Commit history, newest first.
///
/// `--topo-order` is not optional: lane assignment assumes it, and without it
/// branches interleave and the graph is drawn wrong.
pub fn log(repo: &Path, limit: usize) -> Result<Vec<Commit>> {
    let n = limit.to_string();
    let bytes = run(repo, &["log", "--topo-order", "-n", &n, &format!("--format={LOG_FORMAT}")])?;
    Ok(parse_log(&String::from_utf8_lossy(&bytes)))
}

/// A diff. `revspec` is anything git accepts — `HEAD~50..HEAD`, a single sha,
/// `main..feature`. Empty means the working tree against HEAD.
pub fn diff(repo: &Path, revspec: &str) -> Result<Vec<FileDiff>> {
    let bytes = if revspec.is_empty() {
        run(repo, &["diff", "HEAD"])?
    } else if revspec.contains("..") {
        run(repo, &["diff", revspec])?
    } else {
        // A bare revision means "what did this commit change".
        run(repo, &["show", "--format=", revspec])?
    };
    Ok(parse_unified_diff(&String::from_utf8_lossy(&bytes)))
}

/// A short label for the window title.
pub fn describe(repo: &Path) -> String {
    let name = repo.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let branch = run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    if branch.is_empty() { name } else { format!("{name} · {branch}") }
}
