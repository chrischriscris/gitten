//! Getting data out of a real repository.
//!
//! `core` is pure and does no I/O; this crate is the layer that actually talks
//! to git. Today it shells out to the `git` binary for everything. Reads will
//! move to `gix` later for speed — see AGENTS.md — but writes stay here
//! permanently, because shelling out is what gets hooks, credential helpers and
//! `.gitconfig` semantics exactly right.
//!
//! # Why this fetches file contents rather than a diff
//!
//! `git diff` will happily hand over a finished unified diff, and this crate
//! used to parse exactly that. It no longer does, because then *git* chooses the
//! algorithm and `plait_core::differ` is decoration: a semantic or
//! language-aware differ could never be reached, and rule 1 says a built-in may
//! not do anything an extension cannot.
//!
//! So acquisition is two lists of lines per changed file, and which lines
//! correspond is decided in `core` afterwards. `parse_unified_diff` stays for
//! reading `.diff` fixtures off disk, which is a different job.
//!
//! # Two processes, whatever the diff size
//!
//! `git diff --raw` names every changed path and both blob OIDs; `git cat-file
//! --batch` streams every one of those blobs in a single process. A `git show`
//! per file would be a process per file, which on a thousand-file diff is a
//! second of `fork` before any work happens.
//!
//! The OIDs are the reason to want it this way round anyway: a blob's content
//! never changes, so a diff keyed on the pair of them is cacheable forever.

use plait_core::differ::{Differs, Overrides};
use plait_core::{parse_log, Commit, FileDiff};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub type Result<T> = std::result::Result<T, String>;

/// Must match `plait_core::parse_log`.
const LOG_FORMAT: &str = "%H%x1f%h%x1f%P%x1f%an%x1f%at%x1f%s%x1e";

/// An OID of all zeros is git's "not in the object database", which on the new
/// side of a `git diff` means "look in the working tree".
///
/// Tested by shape rather than against a constant because the width is the
/// repository's hash length — 40 for SHA-1, 64 for SHA-256 — and `--raw`
/// abbreviates by default, so the same absence arrives as `0000000` or as forty
/// zeros depending on flags nobody should have to keep in sync with this.
fn is_null_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|b| b == b'0')
}

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

// ------------------------------------------------------------------- the pair

/// One changed file, as the two versions of its text.
///
/// `Vec<String>` and not `&str` into one buffer because the two sides come from
/// different blobs and a rename means the paths differ too. Splitting into lines
/// here rather than in `core` keeps the lossy UTF-8 decode — which is I/O's
/// problem — on this side of the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    /// The path as it is now, which is what the diff is labelled with.
    pub path: String,
    /// The path it had before, when it is not `path`.
    pub old_path: Option<String>,
    /// git's `--raw` status letter: `A`, `M`, `D`, `R`, `C`, `T`.
    pub status: char,
    pub old: Vec<String>,
    pub new: Vec<String>,
    /// Either side contains a NUL byte. Nothing here can usefully diff it, and
    /// the frontend needs to say so rather than draw mojibake.
    pub binary: bool,
}

impl Pair {
    /// How the file is labelled, which for a rename is both names.
    pub fn label(&self) -> String {
        match &self.old_path {
            Some(old) => format!("{old} → {}", self.path),
            None => self.path.clone(),
        }
    }
}

/// Every changed file for `revspec`, as the two versions of its content.
///
/// `revspec` is anything git accepts — `HEAD~50..HEAD`, a single sha,
/// `main..feature`. Empty means the working tree against HEAD.
pub fn pairs(repo: &Path, revspec: &str) -> Result<Vec<Pair>> {
    // `-z` for NUL-separated paths, because a path may contain anything a
    // filesystem allows and git otherwise quotes and escapes it. `-M` so a
    // rename arrives as one file with two names instead of a delete and an add
    // of an identical blob.
    //
    // `--abbrev=64` is load-bearing and looks like a no-op: `--raw` abbreviates
    // OIDs by default, and `cat-file --batch` echoes back the *full* OID in its
    // response header, so an abbreviated request cannot be matched to its
    // answer. 64 is clamped to whatever the repository's hash length actually
    // is, which makes this right for SHA-256 repositories too.
    const RAW: [&str; 5] = ["--raw", "-z", "-M", "--abbrev=64", "--no-ext-diff"];
    let raw = if revspec.is_empty() {
        run(repo, &[&["diff"], &RAW[..], &["HEAD"]].concat())?
    } else if revspec.contains("..") {
        run(repo, &[&["diff"], &RAW[..], &[revspec]].concat())?
    } else {
        // A bare revision means "what did this commit change".
        run(repo, &[&["show"], &RAW[..], &["--format=", revspec]].concat())?
    };

    let changes = parse_raw(&String::from_utf8_lossy(&raw));

    // Every blob the whole diff needs, fetched in one process. `cat-file
    // --batch` keys its answers by the OID it was asked for, so duplicates cost
    // nothing to ask for twice and it is not worth deduplicating here.
    let mut wanted: Vec<&str> = Vec::with_capacity(changes.len() * 2);
    for c in &changes {
        for (mode, oid) in [(&c.old_mode, &c.old_oid), (&c.new_mode, &c.new_oid)] {
            // A gitlink's OID is not in this repository at all; asking for it
            // costs a round trip on a partial clone and gets "missing" anyway.
            if !is_null_oid(oid) && mode != GITLINK {
                wanted.push(oid);
            }
        }
    }
    let blobs = cat_file(repo, &wanted)?;

    let mut out = Vec::with_capacity(changes.len());
    for c in changes {
        // The two sides read a null OID differently, and conflating them is a
        // silent, plausible-looking bug: an added file whose old side falls back
        // to the working tree diffs against itself and shows no change at all.
        let old = Change::synthetic(&c.old_mode, &c.old_oid)
            .or_else(|| blobs.get(&c.old_oid).cloned());
        let new = Change::synthetic(&c.new_mode, &c.new_oid)
            .or_else(|| new_side(&blobs, &c.new_oid, repo, &c.path));
        let binary = old.as_ref().is_some_and(is_binary) || new.as_ref().is_some_and(is_binary);
        out.push(Pair {
            path: c.path,
            old_path: c.old_path,
            status: c.status,
            old: if binary { Vec::new() } else { lines(old.as_deref().unwrap_or_default()) },
            new: if binary { Vec::new() } else { lines(new.as_deref().unwrap_or_default()) },
            binary,
        });
    }
    Ok(out)
}

/// A diff, through whichever [`Differ`](plait_core::differ::Differ) the host
/// routed each path to.
///
/// `over` carries a frontend's live picks — the title-bar dropdowns — and
/// `Overrides::default()` is the configured behaviour. It is here rather than
/// folded into `differs` because the registry belongs to the shared `Host`, which
/// is immutable and replaced wholesale on config reload; names are the only way
/// to say "that registry, these choices" without building a copy of it and losing
/// whatever an extension put in.
///
/// The frontend never learns which implementation ran, and never learns whether
/// the content came from the object database or the working tree.
pub fn diff(
    repo: &Path,
    revspec: &str,
    differs: &Differs,
    over: &Overrides,
) -> Result<Vec<FileDiff>> {
    Ok(pairs(repo, revspec)?
        .iter()
        .map(|p| match p.binary {
            // Modelled as a file with no hunks rather than skipped: the diff
            // still has to say the file changed, and "binary" is the honest
            // thing for it to say.
            true => FileDiff { path: p.label(), hunks: Vec::new() },
            false => {
                let old: Vec<&str> = p.old.iter().map(String::as_str).collect();
                let new: Vec<&str> = p.new.iter().map(String::as_str).collect();
                FileDiff { path: p.label(), ..differs.file_using(over, &p.path, &old, &new) }
            }
        })
        .collect())
}

/// A short label for the window title.
pub fn describe(repo: &Path) -> String {
    // Canonicalised first: `file_name()` of `.` is `None`, and `.` is what every
    // client is given by default — so without this the commonest invocation of
    // all produces a label with the repository's name missing from it.
    let named = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let name = named.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let branch = run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    // `name (branch)` and not `name · branch`: a client puts this after its own
    // name and view, so a third middle dot in one line of chrome reads as four
    // things of equal weight when it is one repository on one branch.
    if branch.is_empty() { name } else { format!("{name} ({branch})") }
}

// ------------------------------------------------------------------- internals

/// A gitlink: `--raw`'s mode for a submodule.
///
/// The OID in that record is a *commit* in another repository, not a blob in
/// this one, so there is nothing to fetch and nothing to diff. git prints a
/// one-line synthetic file instead, and matching it byte for byte is what makes
/// a submodule bump read as a submodule bump rather than as an empty diff.
const GITLINK: &str = "160000";

/// One `--raw` record, before its blobs have been fetched.
#[derive(Debug, PartialEq, Eq)]
struct Change {
    path: String,
    old_path: Option<String>,
    status: char,
    old_mode: String,
    new_mode: String,
    old_oid: String,
    new_oid: String,
}

impl Change {
    /// What one side's content is, when it is not a blob to be fetched.
    fn synthetic(mode: &str, oid: &str) -> Option<Vec<u8>> {
        if mode != GITLINK {
            return None;
        }
        if is_null_oid(oid) {
            return Some(Vec::new());
        }
        Some(format!("Subproject commit {oid}\n").into_bytes())
    }
}

/// `git diff --raw -z` output.
///
/// ```text
/// :100644 100644 a1b2c3… d4e5f6… M\0src/main.rs\0
/// :100644 100644 a1b2c3… d4e5f6… R096\0old/name\0new/name\0
/// ```
///
/// Everything up to the status letter is space-separated; the paths after it are
/// NUL-terminated, and a rename or copy carries two of them. Anything that does
/// not start with `:` is skipped rather than guessed at — `git show` prefixes a
/// commit header that `--format=` does not always suppress.
fn parse_raw(raw: &str) -> Vec<Change> {
    let mut out = Vec::new();
    let mut fields = raw.split('\0').peekable();
    while let Some(meta) = fields.next() {
        let Some(meta) = meta.rsplit('\n').next().and_then(|m| m.strip_prefix(':')) else {
            continue;
        };
        let parts: Vec<&str> = meta.split_whitespace().collect();
        // mode_old mode_new oid_old oid_new status
        if parts.len() < 5 {
            continue;
        }
        let status = parts[4].chars().next().unwrap_or('M');
        let Some(first) = fields.next().filter(|p| !p.is_empty()) else {
            continue;
        };
        // R and C are the only statuses with a second path, and it is the one
        // the file is called now.
        let (old_path, path) = match status {
            'R' | 'C' => match fields.next().filter(|p| !p.is_empty()) {
                Some(second) => (Some(first.to_string()), second.to_string()),
                None => (None, first.to_string()),
            },
            _ => (None, first.to_string()),
        };
        out.push(Change {
            path,
            old_path,
            status,
            old_mode: parts[0].trim_start_matches(':').to_string(),
            new_mode: parts[1].to_string(),
            old_oid: parts[2].to_string(),
            new_oid: parts[3].to_string(),
        });
    }
    out
}

/// Every blob in `oids`, in one `git cat-file --batch`.
///
/// Writing the request list has to happen on another thread. `cat-file` answers
/// as it reads, so a large enough request fills the pipe git is writing into
/// while this process is still filling the pipe git is reading from, and both
/// sides block forever. It is not a rare shape — a thousand-file diff is two
/// thousand OIDs.
fn cat_file(repo: &Path, oids: &[&str]) -> Result<HashMap<String, Vec<u8>>> {
    let mut found = HashMap::new();
    if oids.is_empty() {
        return Ok(found);
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git cat-file: {e}"))?;

    let request: Vec<u8> = oids.iter().flat_map(|o| [o.as_bytes(), b"\n"].concat()).collect();
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&request);
        // Dropping it closes the pipe, which is what tells `cat-file` to exit.
    });

    let out = child.wait_with_output().map_err(|e| format!("git cat-file: {e}"))?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(format!("git cat-file: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }

    // `<oid> SP <type> SP <size> LF <size bytes> LF`, repeated. Bytes
    // throughout: a blob is not text and the header's size is authoritative, so
    // this never has to guess where a record ends.
    let buf = out.stdout;
    let mut i = 0;
    while i < buf.len() {
        let Some(nl) = buf[i..].iter().position(|b| *b == b'\n') else { break };
        let header = String::from_utf8_lossy(&buf[i..i + nl]).into_owned();
        i += nl + 1;
        let parts: Vec<&str> = header.split_whitespace().collect();
        // "<oid> missing" — a blob a shallow or partial clone does not have.
        // Treated as absent rather than as an error: a blobless clone of
        // git/git is a supported fixture, and one unreachable side is still a
        // diff worth showing.
        let (Some(oid), Some(size)) =
            (parts.first(), parts.get(2).and_then(|s| s.parse::<usize>().ok()))
        else {
            continue;
        };
        let end = (i + size).min(buf.len());
        found.insert(oid.to_string(), buf[i..end].to_vec());
        i = end + 1;
    }
    Ok(found)
}

/// The new side's content: from the object database, or from the working tree.
///
/// A null OID here means "not in the object database", which for the new side of
/// a working-tree diff is the ordinary case — the file has been edited and not
/// staged, so what it says now is on disk and nowhere else.
///
/// The old side has no equivalent. A null OID there means the file did not
/// exist, and reading the working tree for it would diff an added file against
/// itself and report that nothing changed.
fn new_side(
    blobs: &HashMap<String, Vec<u8>>,
    oid: &str,
    repo: &Path,
    path: &str,
) -> Option<Vec<u8>> {
    if !is_null_oid(oid) {
        return blobs.get(oid).cloned();
    }
    std::fs::read(repo.join(path)).ok()
}

/// A NUL byte in the first 8 KB, which is git's own test. A real text file does
/// not contain one and every binary format does.
fn is_binary(content: &Vec<u8>) -> bool {
    content.iter().take(8000).any(|b| *b == 0)
}

/// Content into lines.
///
/// **Never `read_to_string`.** Git guarantees no encoding, real history carries
/// Latin-1 author names and `git/git` has commits that are not valid UTF-8 at
/// all. Never fail to show a repo over one bad byte.
///
/// A trailing newline terminates the last line rather than starting an empty
/// one. A file that ends without one is indistinguishable here, which loses
/// git's `\ No newline at end of file` — a gap, and the same one
/// `parse_unified_diff` has.
fn lines(content: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(content);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    if text.is_empty() && content.is_empty() {
        return Vec::new();
    }
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modified_file_is_one_record() {
        let raw = ":100644 100644 aaa bbb M\0src/main.rs\0";
        assert_eq!(
            parse_raw(raw),
            vec![Change {
                path: "src/main.rs".into(),
                old_path: None,
                status: 'M',
                old_mode: "100644".into(),
                new_mode: "100644".into(),
                old_oid: "aaa".into(),
                new_oid: "bbb".into(),
            }]
        );
    }

    #[test]
    fn a_rename_carries_both_names() {
        let raw = ":100644 100644 aaa bbb R096\0old/name.rs\0new/name.rs\0";
        let c = parse_raw(raw);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].old_path.as_deref(), Some("old/name.rs"));
        assert_eq!(c[0].path, "new/name.rs");
        assert_eq!(c[0].status, 'R');
    }

    #[test]
    fn a_null_oid_is_recognised_at_any_width() {
        // `--raw` abbreviates by default and a SHA-256 repository is 64 wide.
        assert!(is_null_oid("0000000"));
        assert!(is_null_oid(&"0".repeat(40)));
        assert!(is_null_oid(&"0".repeat(64)));
        assert!(!is_null_oid(""));
        assert!(!is_null_oid("0000001"));
    }

    #[test]
    fn several_records_run_together() {
        // The trap `-z` sets: records are not newline-separated, so the next
        // record's `:` arrives immediately after the previous path's NUL.
        let raw = ":100644 100644 a b M\0one.rs\0:000000 100644 0000000000000000000000000000000000000000 c A\0two.rs\0:100644 000000 d 0000000000000000000000000000000000000000 D\0three.rs\0";
        let c = parse_raw(raw);
        assert_eq!(c.len(), 3);
        assert_eq!(c.iter().map(|r| r.status).collect::<Vec<_>>(), vec!['M', 'A', 'D']);
        assert_eq!(c[2].path, "three.rs");
    }

    #[test]
    fn a_commit_header_before_the_records_is_skipped() {
        // `git show --format=` still emits a newline before the first record.
        let raw = "\n:100644 100644 a b M\0x.rs\0";
        assert_eq!(parse_raw(raw).len(), 1);
    }

    #[test]
    fn nothing_changed_is_no_records_rather_than_an_error() {
        assert!(parse_raw("").is_empty());
        assert!(parse_raw("\0").is_empty());
        assert!(parse_raw("not a record\0").is_empty());
    }

    #[test]
    fn a_path_with_a_space_survives() {
        // The whole reason for `-z`. Splitting the metadata on whitespace is
        // safe only because the path is not in it.
        let raw = ":100644 100644 a b M\0dir with spaces/a file.rs\0";
        assert_eq!(parse_raw(raw)[0].path, "dir with spaces/a file.rs");
    }

    #[test]
    fn a_submodule_bump_is_a_one_line_synthetic_file() {
        // A gitlink's OID is a commit in another repository. Fetching it as a
        // blob gets "missing", which reads on screen as a file that changed and
        // shows nothing — so it is synthesised the way git does.
        let raw = ":160000 160000 34cbf180d 5697db813 M\0ghostty\0";
        let c = &parse_raw(raw)[0];
        assert_eq!(c.old_mode, "160000");
        let old = Change::synthetic(&c.old_mode, &c.old_oid).expect("a gitlink is synthetic");
        let new = Change::synthetic(&c.new_mode, &c.new_oid).unwrap();
        assert_eq!(lines(&old), vec!["Subproject commit 34cbf180d"]);
        assert_eq!(lines(&new), vec!["Subproject commit 5697db813"]);

        // An added submodule has no old side at all.
        assert_eq!(Change::synthetic("160000", &"0".repeat(40)), Some(Vec::new()));
        // And an ordinary file is not synthetic, so it goes to `cat-file`.
        assert_eq!(Change::synthetic("100644", "aaa"), None);
    }

    #[test]
    fn a_trailing_newline_terminates_rather_than_adds_a_line() {
        assert_eq!(lines(b"a\nb\n"), vec!["a", "b"]);
        assert_eq!(lines(b"a\nb"), vec!["a", "b"]);
        assert_eq!(lines(b""), Vec::<String>::new());
        assert_eq!(lines(b"\n"), vec![""], "a file of one blank line");
        assert_eq!(lines(b"a\r\nb\r\n"), vec!["a", "b"], "CRLF is not part of the line");
    }

    #[test]
    fn a_bad_byte_is_replaced_rather_than_fatal() {
        // `git/git` has commits whose content is not valid UTF-8. Never fail to
        // show a repository over one byte.
        let out = lines(b"caf\xe9 au lait\n");
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("caf"));
    }

    #[test]
    fn binary_is_detected_by_a_nul_byte() {
        assert!(is_binary(&b"\x89PNG\r\n\x1a\n\0\0\0".to_vec()));
        assert!(!is_binary(&b"fn main() {}\n".to_vec()));
        // Past 8 KB it is text as far as this is concerned, which is git's rule.
        let mut late = vec![b'x'; 9000];
        late.push(0);
        assert!(!is_binary(&late));
    }

    #[test]
    fn a_rename_is_labelled_with_both_names() {
        let p = Pair {
            path: "new.rs".into(),
            old_path: Some("old.rs".into()),
            status: 'R',
            old: Vec::new(),
            new: Vec::new(),
            binary: false,
        };
        assert_eq!(p.label(), "old.rs → new.rs");
        assert_eq!(Pair { old_path: None, ..p }.label(), "new.rs");
    }
}
