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
//! algorithm and `gitten_core::differ` is decoration: a semantic or
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

use gitten_core::differ::{Differs, Overrides};
use gitten_core::{parse_log, Commit, FileDiff};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;

pub type Result<T> = std::result::Result<T, String>;

/// Must match `gitten_core::parse_log`.
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

/// The repository's top level. `--raw` and `--porcelain` paths are relative to
/// it, while the `repo` a caller passes may be any subdirectory (the CLI default
/// is the cwd), so working-tree reads must join onto this, not onto `repo`.
fn top_level(repo: &Path) -> PathBuf {
    match run(repo, &["rev-parse", "--show-toplevel"]) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes);
            let trimmed = s.trim();
            if trimmed.is_empty() {
                repo.to_path_buf()
            } else {
                PathBuf::from(trimmed)
            }
        }
        Err(_) => repo.to_path_buf(),
    }
}

/// Commit history, newest first.
///
/// `--topo-order` is not optional: lane assignment assumes it, and without it
/// branches interleave and the graph is drawn wrong.
pub fn log(repo: &Path, limit: usize) -> Result<Vec<Commit>> {
    let n = limit.to_string();
    let bytes = run(
        repo,
        &[
            "log",
            "--topo-order",
            "-n",
            &n,
            &format!("--format={LOG_FORMAT}"),
        ],
    )?;
    Ok(parse_log(&String::from_utf8_lossy(&bytes)))
}

// ------------------------------------------------------------------- the pair

/// One changed file, as the two versions of its text.
///
/// `Vec<Arc<str>>` and not `&str` into one buffer because the two sides come
/// from different blobs and a rename means the paths differ too. Splitting into
/// lines here rather than in `core` keeps the lossy UTF-8 decode — which is I/O's
/// problem — on this side of the boundary. Handles and not owned strings because
/// every changed line flows into a `DiffLine` verbatim: one allocation per line,
/// shared from here to the screen, never copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    /// The path as it is now, which is what the diff is labelled with.
    pub path: String,
    /// The path it had before, when it is not `path`.
    pub old_path: Option<String>,
    /// git's `--raw` status letter: `A`, `M`, `D`, `R`, `C`, `T`.
    pub status: char,
    pub old: Vec<Arc<str>>,
    pub new: Vec<Arc<str>>,
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
///
/// **Collects.** Which means it holds the full text of both sides of every
/// changed file at once, and a whole-history diff of a large repository is
/// gigabytes of that. [`each_pair`] is the same work without the pile, and is
/// what [`diff`] uses; this stays because a test and `diffcheck` genuinely want
/// the list, and at their sizes the pile is free.
pub fn pairs(repo: &Path, revspec: &str) -> Result<Vec<Pair>> {
    let mut out = Vec::new();
    each_pair(repo, revspec, |p| {
        out.push(p);
        Ok(())
    })?;
    Ok(out)
}

/// Every changed file for `revspec`, one at a time, dropped before the next is
/// read.
///
/// # Why this is a callback and not a `Vec`
///
/// [`BlobStream`] is written to hold one file's blobs at a time — the comment on
/// it says the map it replaced "was tens of MB of pure peak overlap" — and then
/// [`pairs`] piled every [`Pair`] into a `Vec` anyway, which put all of it back.
/// Measured, a 29 MB patch peaked at 338 MB and 38 MB of blob content peaked at
/// 107 MB; the incremental read was buying nothing it was not immediately
/// spending.
///
/// It matters most for the shape a diff usually has. A `FileDiff` keeps only the
/// changed lines and their context, so for a one-line fix in a thousand-line file
/// the other 990 lines exist solely to be compared and are garbage the moment the
/// differ has run. Handing the pair over and taking it back is what lets them go.
///
/// A callback rather than an `Iterator` because the work is a state machine over
/// a child process: an iterator would have to own the `BlobStream`, surface its
/// errors per item, and stay alive exactly as long as the borrow of `repo` — all
/// of which is real API surface for a caller that just wants each file once.
/// `f`'s error stops the walk and comes back as this function's, so a consumer
/// can give up early without the process being left half-drained.
///
/// This is also the shape the [roadmap's `Repo` trait](../../docs/roadmap.md)
/// wants: a method that streams what it acquires cannot be retrofitted into one
/// that collects, and the other way round is one line.
pub fn each_pair<F>(repo: &Path, revspec: &str, mut on_pair: F) -> Result<()>
where
    F: FnMut(Pair) -> Result<()>,
{
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
        run(
            repo,
            &[&["diff"], &RAW[..], &["--end-of-options", revspec]].concat(),
        )?
    } else {
        // A bare revision means "what did this commit change".
        //
        // Merges included. Modern git emits no diff at all for a merge unless
        // asked — `git show --raw` prints zero records for one — so a merge
        // commit selected in the log would render as an empty diff, silently.
        // First-parent asks for the ordinary single-old/single-new records this
        // parser already handles; where an older git or `[diff] diffMerges`
        // config instead emits combined-format records, `parse_raw` below
        // refuses them rather than decode them into garbage. First-parent is
        // the honest ordinary answer.
        run(
            repo,
            &[
                &["show"],
                &RAW[..],
                &[
                    "--format=",
                    "--diff-merges=first-parent",
                    "--end-of-options",
                    revspec,
                ],
            ]
            .concat(),
        )?
    };

    let changes = parse_raw(&String::from_utf8_lossy(&raw));

    // Every blob the whole diff needs, fetched by one `cat-file --batch` —
    // but held one file at a time. The batch answers strictly in request
    // order (it reads one OID and writes one answer before reading the
    // next), and requests go out in pair order, old side then new, so the
    // answers can be pulled back per file as each [`Pair`] is built instead
    // of parking every old+new blob of the diff in a map until the last one.
    // On a thousand-file diff that map was tens of MB of pure peak overlap.
    // A duplicate OID costs a second read rather than a second copy, which is
    // the trade the map made implicitly.
    let mut wanted: Vec<&str> = Vec::with_capacity(changes.len() * 2);
    for c in &changes {
        for (mode, oid) in [(&c.old_mode, &c.old_oid), (&c.new_mode, &c.new_oid)] {
            if fetchable(mode, oid) {
                wanted.push(oid);
            }
        }
    }

    // The working-tree pair wants blobs *and* a status, and the two are
    // independent — status reads the index and the working tree, the batch
    // fetches OIDs the diff has already named — so they run side by side and
    // an open of uncommitted work pays one spawn floor instead of two. Nothing
    // is shared between them but `repo`, and neither touches what the other
    // reads. The stream's errors surface first, as `cat-file`'s did when both
    // ran in sequence: a failure to start comes back before any answer is read,
    // and the first failed answer below comes back before `loose` is asked for.
    // A panic in either is resumed rather than swallowed because both calls
    // used to be inline.
    let (blobs, loose) = if revspec.is_empty() {
        std::thread::scope(|s| {
            let loose = s.spawn(|| untracked(repo));
            let blobs = BlobStream::start(repo, &wanted);
            (
                blobs,
                loose
                    .join()
                    .unwrap_or_else(|p| std::panic::resume_unwind(p)),
            )
        })
    } else {
        (BlobStream::start(repo, &wanted), Ok(Vec::new()))
    };
    let mut blobs = blobs?;

    // Untracked files first, so they read as new before the modifications —
    // `git status` lists them last and that is the wrong way round for a diff,
    // where the thing you just created is the thing you are looking for.
    // Fetching them early changed when they arrive, not where they land.
    //
    // These are the one set still gathered before being handed over: `untracked`
    // reads them on another thread while the batch starts, so it has nowhere to
    // hand them to yet. A checkout with thousands of new files is the case that
    // would want the same treatment.
    for p in loose? {
        on_pair(p)?;
    }
    // Working-tree paths from `--raw` are relative to the repo top level, which
    // is not `repo` when the caller passed a subdirectory. Resolve it once here,
    // not per file, and hand it to `new_side` for the on-disk read.
    let root = top_level(repo);
    for c in changes {
        // Both sides pull in request order — old, then new — which is what
        // keeps this loop aligned with the stream.
        //
        // The two sides also read a null OID differently, and conflating them
        // is a silent, plausible-looking bug: an added file whose old side
        // falls back to the working tree diffs against itself and shows no
        // change at all. The old side has no fallback: a null OID there means
        // the file did not exist, and reading the tree for it would diff an
        // added file against itself. On the new side a null OID is the
        // ordinary case of a working-tree diff — what the file says now is on
        // disk and nowhere else.
        let fetched_old = if fetchable(&c.old_mode, &c.old_oid) {
            blobs.answer()?
        } else {
            None
        };
        let fetched_new = if fetchable(&c.new_mode, &c.new_oid) {
            blobs.answer()?
        } else {
            None
        };
        let old = Change::synthetic(&c.old_mode, &c.old_oid).or(fetched_old);
        let new = Change::synthetic(&c.new_mode, &c.new_oid)
            .or(fetched_new)
            .or_else(|| new_side(&c.new_oid, &root, &c.path));
        let binary = old.as_ref().is_some_and(|b| is_binary(b))
            || new.as_ref().is_some_and(|b| is_binary(b));
        // Handed over and gone. What the consumer keeps is its business; what
        // this loop keeps is nothing, which is the whole point.
        on_pair(Pair {
            path: c.path,
            old_path: c.old_path,
            status: c.status,
            old: if binary {
                Vec::new()
            } else {
                lines(old.as_deref().unwrap_or_default())
            },
            new: if binary {
                Vec::new()
            } else {
                lines(new.as_deref().unwrap_or_default())
            },
            binary,
        })?;
    }
    blobs.finish()?;
    Ok(())
}

/// Every untracked file, as a pair with nothing on its old side.
///
/// **`git diff` cannot see these and never will.** It compares the index and the
/// working tree against a commit, and an untracked file is in none of the three
/// — so there is nothing for it to diff. `git status` is the only command that
/// knows they exist, which is why every client that shows them (lazygit, `gh`,
/// every GUI) asks it separately. Without this, "show me my uncommitted work"
/// silently omits every file you just created, which on a real branch is most of
/// what you are looking for.
///
/// Only for the working tree. A revspec compares two commits and neither of them
/// has untracked files in it, so asking would be meaningless.
///
/// Three things it gets right that the obvious version does not:
///
/// - **Ignored files stay out.** `--untracked-files=all` respects `.gitignore`,
///   which is what stops `target/` arriving as forty thousand additions.
/// - **A directory is expanded.** `git status` collapses an untracked directory
///   to `dir/` by default; `all` lists the files inside it, which is what a diff
///   wants.
/// - **A binary file says so** rather than becoming a screenful of NULs, through
///   the same [`is_binary`] test the tracked side uses.
fn untracked(repo: &Path) -> Result<Vec<Pair>> {
    let root = top_level(repo);
    // `-z` for NUL-separated paths, for the reason `--raw -z` has it: a path may
    // contain anything a filesystem allows, and git otherwise quotes and escapes
    // it. `--no-renames` because a rename needs an old side and these have none.
    let raw = run(
        repo,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    let text = String::from_utf8_lossy(&raw);

    let mut out = Vec::new();
    for record in text.split('\0') {
        // `XY path`: two status letters, a space, then the path. Anything that
        // is not `??` is a tracked change and `git diff --raw` already had it.
        let Some(path) = record.strip_prefix("?? ") else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        // Unreadable is skipped rather than an error: a broken symlink, a socket
        // and a file deleted between the two git calls are all untracked, and
        // none of them is worth refusing to show the rest of the diff over.
        let Ok(content) = std::fs::read(root.join(path)) else {
            continue;
        };
        let binary = is_binary(&content);
        out.push(Pair {
            path: path.to_string(),
            old_path: None,
            // The same letter `git diff --raw` uses for a file that was added,
            // so nothing downstream has to learn that untracked is a category.
            status: 'A',
            old: Vec::new(),
            new: if binary { Vec::new() } else { lines(&content) },
            binary,
        });
    }
    Ok(out)
}

/// A diff, through whichever [`Differ`](gitten_core::differ::Differ) the host
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
    // Streamed, not collected. Each pair is diffed and then dropped, so the peak
    // is one file's content plus the whole edit script rather than every file's
    // content plus the whole edit script — see `each_pair` for why that is most
    // of it on a real diff.
    let mut out = Vec::new();
    each_pair(repo, revspec, |p| {
        out.push(match p.binary {
            // Modelled as a file with no hunks rather than skipped: the diff
            // still has to say the file changed, and "binary" is the honest
            // thing for it to say.
            true => FileDiff {
                path: p.label(),
                hunks: Vec::new(),
            },
            false => FileDiff {
                path: p.label(),
                ..differs.file_using(over, &p.path, &p.old, &p.new)
            },
        });
        Ok(())
    })?;
    Ok(out)
}

/// A short label for the window title.
pub fn describe(repo: &Path) -> String {
    // Canonicalised first: `file_name()` of `.` is `None`, and `.` is what every
    // client is given by default — so without this the commonest invocation of
    // all produces a label with the repository's name missing from it.
    let named = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let name = named
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let branch = run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    // `name (branch)` and not `name · branch`: a client puts this after its own
    // name and view, so a third middle dot in one line of chrome reads as four
    // things of equal weight when it is one repository on one branch.
    if branch.is_empty() {
        name
    } else {
        format!("{name} ({branch})")
    }
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
/// NUL-terminated, and a rename or copy carries two of them. Exactly one leading
/// colon is consumed — the rest of the record has no colons. Anything that does
/// not start with `:` is skipped rather than guessed at — `git show` prefixes a
/// commit header that `--format=` does not always suppress.
///
/// A record starting with *two* colons is a combined diff (`::100644 100644
/// 100644 … MM`): git only emits one for a merge when asked, and it carries N
/// modes, N OIDs and an N-letter status. Decoding that into this parser's five
/// positional slots fabricates data — mode in place of OID, a hex digit in place
/// of a status — so such a record is refused, not guessed at. The show path
/// passes `--diff-merges=first-parent`, which keeps git from sending any.
fn parse_raw(raw: &str) -> Vec<Change> {
    let mut out = Vec::new();
    let mut fields = raw.split('\0').peekable();
    while let Some(meta) = fields.next() {
        let Some(meta) = meta.rsplit('\n').next().and_then(|m| m.strip_prefix(':')) else {
            continue;
        };
        // A second leading colon marks a combined record: N modes, N oids and an
        // N-letter status that this fixed-slot parser would read as garbage. With
        // --diff-merges=first-parent git cannot send one; refuse rather than decode.
        if meta.starts_with(':') {
            continue;
        }
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

/// Whether one side of a [`Change`] has a blob on the wire: not a gitlink,
/// whose OID is a *commit* in another repository and gets "missing" back, and
/// not a null OID, which means "not in the object database". The exact
/// predicate that builds the request list, asked again while consuming so the
/// answers stay paired with their file.
fn fetchable(mode: &str, oid: &str) -> bool {
    !is_null_oid(oid) && mode != GITLINK
}

/// One `git cat-file --batch`, answered in request order.
///
/// Writing the request list has to happen on another thread. `cat-file` answers
/// as it reads, so a large enough request fills the pipe git is writing into
/// while this process is still filling the pipe git is reading from, and both
/// sides block forever. It is not a rare shape — a thousand-file diff is two
/// thousand OIDs.
///
/// Reading stays incremental rather than slurping the whole output: each
/// answer is parsed and handed over as it arrives, so a file's blobs can be
/// dropped before the next file's are read.
struct BlobStream {
    child: Option<Child>,
    reader: Option<BufReader<ChildStdout>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
}

impl BlobStream {
    /// Starts the batch. Nothing was asked for, nothing is started — a clean
    /// working tree takes no process at all.
    fn start(repo: &Path, oids: &[&str]) -> Result<Self> {
        if oids.is_empty() {
            return Ok(Self {
                child: None,
                reader: None,
                stderr: None,
                writer: None,
            });
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

        // One buffer for the whole request: a `[oid, b"\n"].concat()` per OID
        // was an allocation each, for bytes written once and never read again.
        let mut request = Vec::with_capacity(oids.iter().map(|o| o.len() + 1).sum::<usize>());
        for o in oids {
            request.extend_from_slice(o.as_bytes());
            request.push(b'\n');
        }
        let mut stdin = child.stdin.take().expect("piped");
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&request);
            // Dropping it closes the pipe, which is what tells `cat-file` to exit.
        });

        let stderr = child.stderr.take().expect("piped");
        // Drained concurrently: an error long enough to fill the pipe must not
        // be able to block git while nobody is reading its answer stream yet.
        let stderr = std::thread::spawn(move || {
            let mut err = Vec::new();
            let mut stderr = stderr;
            let _ = stderr.read_to_end(&mut err);
            err
        });
        let stdout = child.stdout.take().expect("piped");
        Ok(Self {
            child: Some(child),
            reader: Some(BufReader::new(stdout)),
            stderr: Some(stderr),
            writer: Some(writer),
        })
    }

    /// The next answer in request order.
    ///
    /// `<oid> SP <type> SP <size> LF <size bytes> LF`, or `<oid> SP missing LF`
    /// for a blob a shallow or partial clone does not have. Bytes throughout:
    /// a blob is not text and the header's size is authoritative, so this
    /// never has to guess where a record ends.
    fn answer(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(stream) = self.reader.as_mut() else {
            return Ok(None);
        };
        let mut header = Vec::new();
        let read = stream
            .read_until(b'\n', &mut header)
            .map_err(|e| format!("git cat-file: {e}"))?;
        if read == 0 {
            // Every requested answer was written before any of this could run;
            // ending early is a broken protocol, not an empty diff.
            return Err("git cat-file: unexpected end of output".into());
        }
        let header = String::from_utf8_lossy(&header).into_owned();
        let parts: Vec<&str> = header.split_whitespace().collect();
        // "<oid> missing" — treated as absent rather than as an error: a
        // blobless clone of git/git is a supported fixture, and one unreachable
        // side is still a diff worth showing.
        let Some(size) = parts.get(2).and_then(|s| s.parse::<usize>().ok()) else {
            return Ok(None);
        };
        let mut content = vec![0; size];
        stream
            .read_exact(&mut content)
            .map_err(|e| format!("git cat-file: {e}"))?;
        let mut terminator = [0; 1];
        stream
            .read_exact(&mut terminator)
            .map_err(|e| format!("git cat-file: {e}"))?;
        Ok(Some(content))
    }

    /// Waits for git to exit and reports failure the way a slurped run did:
    /// closing our end of the pipe first, so git cannot block on answers
    /// nobody will read.
    fn finish(mut self) -> Result<()> {
        self.close()?;
        Ok(())
    }

    /// Shared by `finish` and `Drop`, because an early error leaves the same
    /// three things behind as a completed run does.
    fn close(&mut self) -> Result<()> {
        drop(self.reader.take()); // EOF for git, and no pipe left to block on
        let err = self
            .stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        if let Some(mut child) = self.child.take() {
            let status = child.wait().map_err(|e| format!("git cat-file: {e}"))?;
            let _ = self.writer.take().and_then(|h| h.join().ok());
            if !status.success() {
                return Err(format!(
                    "git cat-file: {}",
                    String::from_utf8_lossy(&err).trim()
                ));
            }
        }
        Ok(())
    }
}

impl Drop for BlobStream {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// The new side's content when the object database had nothing: from the
/// working tree.
///
/// A null OID here means "not in the object database", which for the new side of
/// a working-tree diff is the ordinary case — the file has been edited and not
/// staged, so what it says now is on disk and nowhere else.
///
/// The old side has no equivalent. A null OID there means the file did not
/// exist, and reading the working tree for it would diff an added file against
/// itself and report that nothing changed.
fn new_side(oid: &str, root: &Path, path: &str) -> Option<Vec<u8>> {
    if !is_null_oid(oid) {
        return None;
    }
    std::fs::read(root.join(path)).ok()
}

/// A NUL byte in the first 8 KB, which is git's own test. A real text file does
/// not contain one and every binary format does.
fn is_binary(content: &[u8]) -> bool {
    content.iter().take(8000).any(|b| *b == 0)
}

/// Content into lines, one shared handle each.
///
/// **Never `read_to_string`.** Git guarantees no encoding, real history carries
/// Latin-1 author names and `git/git` has commits that are not valid UTF-8 at
/// all. Never fail to show a repo over one bad byte.
///
/// A trailing newline terminates the last line rather than starting an empty
/// one. A file that ends without one is indistinguishable here, which loses
/// git's `\ No newline at end of file` — a gap, and the same one
/// `parse_unified_diff` has.
///
/// **The carriage return of a CRLF line stays in the line.** Stripping it here
/// is the plausible-looking bug: `\r` is *content*, git diffs it, and acquisition
/// is not the layer that gets to decide it does not count. A commit that
/// converts a file's endings then arrived as a file with no changes in it — git
/// reporting three insertions and three deletions where this reported `+0 -0`,
/// which reads exactly like a binary file and is the one shape a diff viewer must
/// never produce. It also disagreed with [`crate::parse_unified_diff`], which
/// keeps the byte: the same commit read one way from a repository and another
/// from a `.diff` of itself.
///
/// Which leaves the presentation of a control character to a presentation, where
/// it belongs — the terminal already substitutes `·` for one. And it puts the
/// *choice* where the rule says it goes: ignoring a `\r` is
/// [`Whitespace`](gitten_core::differ::Whitespace)'s to make, and every mode
/// above `Exact` trims it for free because `\r` is whitespace.
fn lines(content: &[u8]) -> Vec<Arc<str>> {
    let text = String::from_utf8_lossy(content);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    if text.is_empty() && content.is_empty() {
        return Vec::new();
    }
    text.split('\n').map(Arc::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::differ::Whitespace;

    /// A throwaway repository, because untracked files are the one thing that
    /// cannot be tested against a canned `--raw` string: they are defined by
    /// *not* being in git, so only a real working tree has any.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("gitten-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let me = Scratch(dir);
            me.git(&["init", "-q", "."]);
            me
        }

        fn git(&self, args: &[&str]) {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        fn write(&self, path: &str, content: &[u8]) {
            let at = self.0.join(path);
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent).expect("a parent");
            }
            std::fs::write(at, content).expect("a file");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn paths(pairs: &[Pair]) -> Vec<&str> {
        pairs.iter().map(|p| p.path.as_str()).collect()
    }

    /// The lines as plain slices, for assertions.
    fn strs(lines: &[Arc<str>]) -> Vec<&str> {
        lines.iter().map(|l| l.as_ref()).collect()
    }

    #[test]
    fn an_untracked_file_is_a_pair_with_nothing_opposite_it() {
        // The whole point: `git diff` cannot see these, so without the separate
        // `git status` pass "show me my uncommitted work" omits every file you
        // just created.
        let r = Scratch::new("untracked");
        r.write("tracked.txt", b"one\ntwo\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("tracked.txt", b"one\nCHANGED\n");
        r.write("new.txt", b"brand new\nsecond\n");

        let got = pairs(&r.0, "").expect("a working tree diff");
        assert_eq!(
            paths(&got),
            vec!["new.txt", "tracked.txt"],
            "untracked comes first"
        );
        let new = &got[0];
        assert_eq!(
            new.status, 'A',
            "an untracked file is an addition like any other"
        );
        assert!(new.old.is_empty(), "it has no old side");
        assert_eq!(strs(&new.new), ["brand new", "second"]);
        assert!(!new.binary);
    }

    #[test]
    fn a_working_tree_diff_is_correct_from_a_subdirectory() {
        // `--raw` and `--porcelain` paths are relative to the repo top level, but
        // the `repo` a caller passes is the cwd by default — a subdirectory. Join
        // a root-relative path onto that subdirectory and the on-disk read fails:
        // a modification reads as a full deletion (empty new side) and an
        // untracked file is skipped entirely. Both look like plausible output.
        let r = Scratch::new("subdir");
        r.write("sub/a.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("sub/a.txt", b"two\n");
        r.write("sub/new.txt", b"fresh\n");

        // Pass the subdirectory as `repo`, the way the cwd default does.
        let got = pairs(&r.0.join("sub"), "").expect("a working tree diff");

        let names = paths(&got);
        assert!(
            names.contains(&"sub/new.txt"),
            "the untracked file must appear, not be silently skipped: {names:?}"
        );
        let modified = got
            .iter()
            .find(|p| p.path == "sub/a.txt")
            .expect("the modified file is in the diff");
        assert_eq!(
            strs(&modified.new),
            ["two"],
            "the modification keeps its new side rather than reading as a deletion"
        );
    }

    #[test]
    fn an_ignored_file_stays_out() {
        // `--untracked-files=all` respects `.gitignore`, which is what stops
        // `target/` arriving as forty thousand additions.
        let r = Scratch::new("ignored");
        r.write(".gitignore", b"skip/\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("skip/junk.txt", b"noise\n");
        r.write("kept.txt", b"yes\n");

        assert_eq!(paths(&pairs(&r.0, "").unwrap()), vec!["kept.txt"]);
    }

    #[test]
    fn an_untracked_directory_is_expanded_into_its_files() {
        // `git status` collapses one to `dir/` by default, and a diff of a
        // directory is not a thing. `--untracked-files=all` is what asks for
        // the files inside it.
        let r = Scratch::new("deep");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("a/b/c.txt", b"buried\n");

        assert_eq!(paths(&pairs(&r.0, "").unwrap()), vec!["a/b/c.txt"]);
    }

    #[test]
    fn a_path_with_a_space_survives_because_the_records_are_nul_separated() {
        // `git status` *quotes* such a path in its normal output. `-z` is what
        // stops it, and without it the pair would be named `"has space.txt"`
        // — quotes included — and the file read would fail.
        let r = Scratch::new("spaced");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("has space.txt", b"spaced\n");

        let got = pairs(&r.0, "").unwrap();
        assert_eq!(paths(&got), vec!["has space.txt"]);
        assert_eq!(strs(&got[0].new), ["spaced"]);
    }

    #[test]
    fn a_revspec_cannot_smuggle_an_option_to_git() {
        // A revspec beginning with `-` would be parsed by git as an option
        // without `--end-of-options`, and `--output=<path>` would make
        // `git diff` write to an arbitrary file. The separator must turn it
        // back into a (nonexistent) revision, so the file is never written.
        let r = Scratch::new("smuggle");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);

        let target = r.0.join("PWNED");
        let hostile = format!("--output={}", target.display());

        // The `..` (diff) arm.
        let _ = pairs(&r.0, &format!("{hostile}..HEAD"));
        assert!(
            !target.exists(),
            "a revspec must not be able to make git write a file"
        );

        // The bare-revision (show) arm.
        let _ = pairs(&r.0, &hostile);
        assert!(
            !target.exists(),
            "the bare-revision arm must guard the separator too"
        );
    }

    #[test]
    fn an_untracked_binary_says_so_rather_than_becoming_nul_soup() {
        let r = Scratch::new("binary");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("blob.png", b"\x89PNG\x00\x00rest");

        let got = pairs(&r.0, "").unwrap();
        assert_eq!(paths(&got), vec!["blob.png"]);
        assert!(got[0].binary);
        assert!(got[0].new.is_empty(), "a binary carries no lines");
    }

    #[test]
    fn a_revspec_asks_for_no_untracked_files() {
        // Two commits, and neither of them has untracked files in it. Including
        // the working tree's would put files in a diff of history that are not
        // in that history.
        let r = Scratch::new("revspec");
        r.write("a.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "first"]);
        r.write("a.txt", b"two\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "second"]);
        r.write("loose.txt", b"not in history\n");

        assert_eq!(paths(&pairs(&r.0, "HEAD~1..HEAD").unwrap()), vec!["a.txt"]);
        assert!(
            paths(&pairs(&r.0, "").unwrap()).contains(&"loose.txt"),
            "the working tree has it"
        );
    }

    #[test]
    fn blobs_arrive_with_their_own_file_when_the_stream_is_consumed_in_order() {
        // The batch is answered in request order and consumed one file at a
        // time; if that pairing ever slipped by one, contents would swap
        // between files instead of anything failing loudly.
        let r = Scratch::new("ordered");
        r.write("a.txt", b"original a\n");
        r.write("b.txt", b"original b\n");
        r.write("c.txt", b"original c\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("a.txt", b"A\n");
        r.write("b.txt", b"B\n");
        // Identical content is an identical OID: the same blob asked for twice,
        // which must still land on both of its files.
        r.write("c.txt", b"same bytes\n");
        r.write("d.txt", b"same bytes\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "second"]);

        let got = pairs(&r.0, "HEAD~1..HEAD").unwrap();
        assert_eq!(paths(&got), vec!["a.txt", "b.txt", "c.txt", "d.txt"]);
        let new: Vec<_> = got.iter().map(|p| p.new.join("\n")).collect();
        assert_eq!(new, vec!["A", "B", "same bytes", "same bytes"]);
    }

    #[test]
    fn a_merge_commit_diffs_against_its_first_parent() {
        // Modern git emits no `--raw` records at all for a merge, which made a
        // selected merge commit render as nothing. The show path asks for
        // first-parent instead, so what arrives here has to be ordinary
        // single-colon records agreeing with git's own answer between parent
        // one and the merge — and never a positionally-decoded combined record,
        // whose tell would be hex digits where status letters belong.
        let r = Scratch::new("merge");
        r.write("a.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "a"]);
        r.git(&["checkout", "-qb", "side"]);
        r.write("a.txt", b"one\nCHANGED\n");
        r.write("side.txt", b"fresh\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "b"]);
        r.git(&["checkout", "-q", "-"]);
        // Flags un-bundled: git only lets a short option eat a value when it
        // is attached, so `-qm msg` leaves -m empty and `msg` as the ref.
        r.git(&["merge", "--no-ff", "-q", "-m", "merge side", "side"]);

        let git = |args: &[&str]| -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(&r.0)
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        let merge = git(&["rev-parse", "HEAD"]).trim().to_string();
        // `--no-ff` guaranteed a true merge commit: exactly two parents.
        assert_eq!(
            git(&["log", "-1", "--format=%P", &merge])
                .split_whitespace()
                .count(),
            2,
            "the fixture really did produce a merge"
        );

        let got = pairs(&r.0, &merge).expect("a diff for a merge commit");
        assert!(!got.is_empty(), "a merge renders as a diff, not silence");

        // Git's own answer to the same question: ordinary records between
        // parent one and the merge, parsed with the same parser rather than
        // reimplementing its record format here.
        let expected = parse_raw(&git(&[
            "diff",
            "--raw",
            "-z",
            "-M",
            "--no-ext-diff",
            &format!("{merge}^1"),
            &merge,
        ]));
        let want: std::collections::BTreeSet<String> =
            expected.iter().map(|c| c.path.clone()).collect();
        let have: std::collections::BTreeSet<String> = got.iter().map(|p| p.path.clone()).collect();
        assert_eq!(have, want, "paths must match git's own first-parent diff");

        for p in &got {
            assert!(
                p.status.is_alphabetic(),
                "{}: `{}` is a hex-garbage status, not git's letter",
                p.path,
                p.status
            );
        }
    }

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
        assert_eq!(
            c.iter().map(|r| r.status).collect::<Vec<_>>(),
            vec!['M', 'A', 'D']
        );
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
    fn a_combined_record_is_refused_rather_than_misdecoded() {
        // Two colons mark a combined merge record: N modes, N oids and an
        // N-letter status. Read into this parser's five slots, `"100644"`
        // lands in old_oid, a hex digit becomes the status and whichever blob
        // sits in slot three gets drawn as real content. One colon is
        // consumed; a second is a refusal — and because every record is one
        // `\0`-separated chunk, a skipped record leaves the well-formed one
        // after it intact.
        let raw = concat!(
            "::100644 100644 100644 aaaa bbbb cccc MM\0src/main.rs\0",
            ":100644 100644 aaa bbb M\0keep.txt\0",
        );
        assert_eq!(
            parse_raw(raw),
            vec![Change {
                path: "keep.txt".into(),
                old_path: None,
                status: 'M',
                old_mode: "100644".into(),
                new_mode: "100644".into(),
                old_oid: "aaa".into(),
                new_oid: "bbb".into(),
            }],
            "the combined record never becomes a Change and no field of it leaks"
        );
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
        assert_eq!(strs(&lines(&old)), ["Subproject commit 34cbf180d"]);
        assert_eq!(strs(&lines(&new)), ["Subproject commit 5697db813"]);

        // An added submodule has no old side at all.
        assert_eq!(
            Change::synthetic("160000", &"0".repeat(40)),
            Some(Vec::new())
        );
        // And an ordinary file is not synthetic, so it goes to `cat-file`.
        assert_eq!(Change::synthetic("100644", "aaa"), None);
    }

    #[test]
    fn a_trailing_newline_terminates_rather_than_adds_a_line() {
        assert_eq!(strs(&lines(b"a\nb\n")), ["a", "b"]);
        assert_eq!(strs(&lines(b"a\nb")), ["a", "b"]);
        assert_eq!(lines(b""), Vec::<Arc<str>>::new());
        assert_eq!(strs(&lines(b"\n")), [""], "a file of one blank line");
    }

    #[test]
    fn a_carriage_return_stays_in_the_line() {
        // The whole of the CRLF bug in one assertion. Strip it here and a commit
        // that converts a file's line endings has nothing in it: every line
        // compares equal, the differ finds no edits, and the file draws as
        // `+0 -0` with no hunks — indistinguishable from a binary file.
        assert_eq!(strs(&lines(b"a\r\nb\r\n")), ["a\r", "b\r"]);
        // And the two sides then differ, which is the point.
        assert_ne!(lines(b"a\n"), lines(b"a\r\n"));
    }

    #[test]
    fn ignoring_a_carriage_return_is_the_whitespace_relations_job() {
        // `Exact` sees it — that is what makes the diff agree with git. Every
        // mode above `Exact` trims it, because `\r` is whitespace, so
        // `--ignore-space-at-eol` collapses a line-ending change exactly as
        // git's does. No mode in between, and nothing hardcoded in acquisition.
        let lf = lines(b"alpha\nbeta\n");
        let crlf = lines(b"alpha\r\nbeta\r\n");
        let differs = Differs::default();
        let edits = |ws| {
            differs
                .file_using(
                    &Overrides {
                        whitespace: Some(ws),
                        ..Default::default()
                    },
                    "f.txt",
                    &lf,
                    &crlf,
                )
                .hunks
                .len()
        };
        assert_eq!(edits(Whitespace::Exact), 1, "exact must see the change");
        for ws in [Whitespace::Trailing, Whitespace::Change, Whitespace::All] {
            assert_eq!(edits(ws), 0, "{} did not trim the CR", ws.name());
        }
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
        assert!(is_binary(b"\x89PNG\r\n\x1a\n\0\0\0"));
        assert!(!is_binary(b"fn main() {}\n"));
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
        assert_eq!(
            Pair {
                old_path: None,
                ..p
            }
            .label(),
            "new.rs"
        );
    }
}
