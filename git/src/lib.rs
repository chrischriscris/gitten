//! Getting data out of a real repository.
//!
//! `core` is pure and does no I/O; this crate is the layer that actually talks
//! to git. Today it shells out to the `git` binary for everything. Reads will
//! move to `gix` later for speed — see AGENTS.md — but writes stay here
//! permanently, because shelling out is what gets hooks, credential helpers and
//! `.gitconfig` semantics exactly right.
//!
//! # One surface: [`Repo`]
//!
//! Free functions grew here first because there was nothing to hide behind.
//! Now every read goes through the [`Repo`] trait, held as a shared
//! [`Handle`], and the binary-backed implementation is private: an
//! implementation may be this subprocess, gix later, or a test's fake, and no
//! caller can tell which ran. That is also what makes the reads swappable per
//! repository rather than per process — one handle each, opened once, kept for
//! the life of a client.
//!
//! Diff *assembly* is deliberately not on the trait. [`diff`] turns whatever
//! any implementation's `pairs` into `FileDiff`s through the configured
//! differ registry, and no `Repo` may substitute its own algorithm — see that
//! function for why.
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
use gitten_core::refs::{
    Branch, HeadState, ReflogEntry, Remote, RemoteBranch, ResetMode, Stash, Tag, Upstream,
};
use gitten_core::status::{
    Change, ConflictEntry, ConflictKind, Kind, PathBytes, StagedEntry, Status, Submodule,
    UnstagedEntry, UntrackedEntry,
};
use gitten_core::{parse_log, Commit, FileDiff};

/// The interactive-rebase plan, re-exported because it appears on the
/// [`Repo`] trait: an implementor should not need to know which crate
/// spelled it.
pub use gitten_core::rebase::TodoScript;
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

/// [`run`] for arguments that are names rather than text.
///
/// A branch name may carry bytes no UTF-8 string can hold, and handing git
/// their decoded near-misses would read somebody else's ref — the same
/// reason [`join_raw`] exists for paths. Same process shape, byte arguments.
fn run_bytes(repo: &Path, args: &[&[u8]]) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args.iter().map(|a| std::ffi::OsStr::from_bytes(a)))
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        let err = args
            .iter()
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        return Err(format!(
            "git {err}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// [`run_bytes`] with one argument's worth of bytes riding **stdin**.
///
/// A patch is arbitrary text — newlines, quotes, whatever encoding the diff
/// carried — and argv is none of those things. The same transport
/// [`Binary::commit_via`] uses for commit messages, generalized: write the
/// payload, close the pipe (EOF is its end), and let git's exit status tell
/// the story. A write failure here — a child that quit before reading — is
/// reported through that status with better words than ours would be.
fn run_stdin(repo: &Path, args: &[&[u8]], input: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args.iter().map(|a| std::ffi::OsStr::from_bytes(a)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let _ = stdin.write_all(input);
    drop(stdin);
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git {}: {e}", display_args(args)))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            display_args(args),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// The arguments as a person reads them in an error line — flags and names,
/// never the stdin payload, which is prose's job to quote or not.
fn display_args(args: &[&[u8]]) -> String {
    args.iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// [`run_bytes`] with environment set for the one process.
///
/// The rebase verbs are its only callers tonight, and for a reason worth
/// keeping rare: environment is process-wide state by another name, and the
/// one legitimate use here is pointing git's *editor* at something that
/// answers without a human — never at changing what git itself reads.
fn run_env(repo: &Path, args: &[&[u8]], env: &[(&str, &str)]) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let mut command = Command::new("git");
    command.arg("-C").arg(repo);
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command
        .args(args.iter().map(|a| std::ffi::OsStr::from_bytes(a)))
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            display_args(args),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Writes a plan to one freshly created temp file and names it.
///
/// Three properties, because this file carries commit subjects and lives in
/// a directory other users may be able to write:
///
/// **`create_new`** — the create fails if anything already sits at the
/// path, so a pre-planted file or symlink cannot be clobbered with a plan
/// it did not hold (CWE-377's classic shape); we simply pick another name.
///
/// **An unguessable name** — process id, clock nanoseconds and a sequence
/// counter together, so nobody can win the race by predicting where the
/// next plan will land.
///
/// **`0600`** — the owner reads it, and nobody else, whatever the umask or
/// the platform default for new files would have said.
///
/// The file exists only for the length of the rebase process; uniqueness
/// is per call rather than per process, because two rebases queued behind
/// each other on the job thread must not share a plan.
fn write_todo_tmpfile(script: Vec<u8>) -> Result<PathBuf> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.subsec_nanos())
        .unwrap_or(0);
    for attempt in 0..4 {
        let mut at = std::env::temp_dir();
        at.push(format!(
            "gitten-todo-{}-{:x}-{:x}-{:x}",
            std::process::id(),
            nanos,
            attempt,
            SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&at)
        {
            Ok(file) => {
                // Private before the first byte lands, not after.
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("could not lock down {}: {e}", at.display()))?;
                (&file)
                    .write_all(&script)
                    .map_err(|e| format!("could not write {}: {e}", at.display()))?;
                return Ok(at);
            }
            // Somebody (or something) got there first: another name, not a
            // fight over theirs.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not create {}: {e}", at.display())),
        }
    }
    Err("could not create a private todo tempfile after four attempts".into())
}

/// A path as one shell word. Temp directories do not usually need the
/// quotes; a machine whose `$TMPDIR` has a space in it does, and quoting a
/// path that did not need it costs nothing.
///
/// The single-quote form cannot represent an embedded quote itself, so those
/// are spelled the standard way: close, an escaped literal quote, reopen.
fn shell_quote(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    format!("'{}'", text.replace('\'', r"'\''"))
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

/// Joins a raw pathname onto the repository root, byte for byte.
///
/// Unix keeps pathnames as opaque bytes and macOS and Linux agree on that, so
/// the bytes go straight into an [`std::ffi::OsStr`] and through to the
/// filesystem. Decoding lossily first — the obvious `root.join(path.to_string_lossy())`
/// — stats a *different* file than git named whenever a name carries a byte
/// that is not UTF-8: the read misses, and the diff silently shows nothing
/// where somebody's file is. Display is the only place a decode belongs; this
/// is the addressing form.
fn join_raw(root: &Path, path: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    root.join(std::ffi::OsStr::from_bytes(path))
}

/// The byte budget of paths one bulk-write process is handed, counted after
/// the fixed head of its argv.
///
/// Bulk exists for the huge case — stage-all on a fresh tree — which is
/// exactly where a single argv overflows `E2BIG`. Past the budget the
/// remainder simply becomes the next process: rare, sequential, and invisible
/// next to the fork cost it saves.
const ARGV_BUDGET: usize = 100 * 1024;

/// The exclusive end of one process's worth of `paths`, starting at `at`.
///
/// Always at least one path — a single name larger than the budget still
/// travels, because progress beats deadlock — then every neighbour that fits
/// under [`ARGV_BUDGET`] beside it.
fn chunk_end(paths: &[&[u8]], at: usize) -> usize {
    let mut bytes = 0;
    let mut end = at;
    while end < paths.len() {
        bytes += paths[end].len();
        if end > at && bytes > ARGV_BUDGET {
            break;
        }
        end += 1;
    }
    end
}

// ------------------------------------------------------------------- the seam

/// One repository, as everything behind a client sees it.
///
/// Reads only, for now — writes become verbs on this same trait when they
/// exist, which is the point of having it. Nothing here hands out a path or a
/// process, so an implementation can be this crate's subprocess, gix, or a
/// test's fake, and no frontend is ever the wiser.
///
/// Object-safe on purpose, and never generic over its implementation: callers
/// hold a [`Handle`] — a reference — so one opened repository serves every
/// view, reload and re-acquire a client makes without threading a concrete
/// type through all of them.
pub trait Repo: Send + Sync {
    /// Commit history, newest first.
    ///
    /// `--topo-order` is not optional: lane assignment assumes it, and without
    /// it branches interleave and the graph is drawn wrong.
    fn log(&self, limit: usize) -> Result<Vec<Commit>>;

    /// Every changed file for `revspec`, as the two versions of its content.
    ///
    /// `revspec` is anything git accepts — `HEAD~50..HEAD`, a single sha,
    /// `main..feature`. Empty means the working tree against HEAD, with
    /// untracked files included; a revspec compares two commits, and neither
    /// has untracked files in it.
    fn pairs(&self, revspec: &str) -> Result<Vec<Pair>>;

    /// The working tree against HEAD and the index: staged, unstaged,
    /// untracked and conflicted, each list its own answer. See
    /// [`gitten_core::status`] for the model and why the four are separate.
    fn status(&self) -> Result<Status>;

    /// The local branches, with HEAD's position and each branch's upstream
    /// counts. See [`gitten_core::refs`] for the model.
    fn branches(&self) -> Result<Vec<Branch>> {
        Err(unserved("branches"))
    }

    /// The branches as the remotes hold them, as of the last fetch. See
    /// [`gitten_core::refs`].
    fn remote_branches(&self) -> Result<Vec<RemoteBranch>> {
        Err(unserved("remote branches"))
    }

    /// Where `HEAD` points — a branch, or a commit it detached onto, or
    /// nothing at all in a repository with no commits yet. Detached is a
    /// state here and never an error; see [`HeadState`].
    fn head(&self) -> Result<HeadState> {
        Err(unserved("HEAD"))
    }

    /// The stash stack, newest first.
    fn stashes(&self) -> Result<Vec<Stash>> {
        Err(unserved("stashes"))
    }

    /// The remotes this repository knows by name, with their URLs.
    fn remotes(&self) -> Result<Vec<Remote>> {
        Err(unserved("remotes"))
    }

    /// The tags, each resolved to the commit it names.
    fn tags(&self) -> Result<Vec<Tag>> {
        Err(unserved("tags"))
    }

    /// Where HEAD has been: up to `limit` entries, newest first. Zero asks
    /// for none.
    fn reflog(&self, _limit: usize) -> Result<Vec<ReflogEntry>> {
        Err(unserved("the reflog"))
    }

    // --------------------------------------------------------------- the verbs

    /// Stages one path: `git add --`.
    ///
    /// One word for everything the working tree can be doing to a file —
    /// untracked, modified, deleted — because that is git's own answer: `add`
    /// records a path's current state in the index whatever state it is. The
    /// path is bytes, exactly as [`status`](Self::status) named it.
    fn stage(&self, _path: &[u8]) -> Result<()> {
        Err(unserved("staging"))
    }

    /// Unstages one path: `git reset` against HEAD.
    ///
    /// The change returns to wherever it came from — a staged modification
    /// back to unstaged, a newly added file back to untracked — and the
    /// working tree is never touched. Nothing staged for the path is not an
    /// error: git answers a no-op, and so does this.
    fn unstage(&self, _path: &[u8]) -> Result<()> {
        Err(unserved("unstaging"))
    }

    /// Checks out one path's working-tree state away —
    /// `git checkout -- <path>`.
    ///
    /// The destructive half of staging's opposite: the working tree returns
    /// to whatever the *index* holds, which is why a staged modification
    /// survives and an unstaged one does not. Nothing staged is touched, so
    /// this is one verb short of lazygit's whole-file discard — unstage first
    /// when the index has to lose it too. Bytes throughout, exactly as
    /// [`status`](Self::status) named the path.
    fn discard(&self, _path: &[u8]) -> Result<()> {
        Err(unserved("discarding"))
    }

    /// Deletes one untracked file from the working tree.
    ///
    /// A separate word and not a branch of [`discard`](Self::discard)
    /// because it is a different destruction with no git plumbing behind it:
    /// an untracked file is in no commit and no index, so there is nothing
    /// to check out — the file simply stops existing. The caller knows which
    /// kind it holds from [`status`](Self::status) and aims accordingly;
    /// folding both under one name would hide "this press deletes the file"
    /// inside a verb whose usual mechanics are reversible.
    fn remove_untracked(&self, _path: &[u8]) -> Result<()> {
        Err(unserved("untracked removal"))
    }

    /// Appends one path to the repository's root `.gitignore`, creating the
    /// file when it does not exist.
    ///
    /// The line is anchored (`/name`) so it matches from the root rather
    /// than at any depth, escaped where git would otherwise read pattern
    /// syntax into it, and written only once — ignoring twice leaves the
    /// file byte-identical. A name holding a line break is refused in
    /// words rather than answered with a pattern that cannot match
    /// anything. Nothing else happens here: the entry stays on disk,
    /// untracked but now ignored, and disappears from
    /// [`status`](Self::status)'s untracked list on the next read because
    /// git stops listing ignored files on its own.
    fn ignore(&self, _path: &[u8]) -> Result<()> {
        Err(unserved("ignoring"))
    }

    /// Stages every path in one call.
    ///
    /// Bulk spelling of [`stage`](Self::stage), for the stage-everything
    /// command: the binary-backed implementation answers it with `add`
    /// processes over the whole list instead of one process per path, and a
    /// backend that does not need the distinction gets the loop free through
    /// this default. Empty is a quiet no-op.
    fn stage_many(&self, paths: &[&[u8]]) -> Result<()> {
        for path in paths {
            self.stage(path)?;
        }
        Ok(())
    }

    /// Unstages every path in one call — the bulk spelling of
    /// [`unstage`](Self::unstage), on the same terms as
    /// [`stage_many`](Self::stage_many).
    fn unstage_many(&self, paths: &[&[u8]]) -> Result<()> {
        for path in paths {
            self.unstage(path)?;
        }
        Ok(())
    }

    /// Stages exactly what `patch` describes onto the index:
    /// `git apply --cached`.
    ///
    /// The patch arrives as bytes and rides **stdin**, never argv — a patch
    /// is arbitrary text with its own newlines, quoting rules and encodings,
    /// which is argv's idea of nothing. Synthesis lives in
    /// [`gitten_core::patch`](gitten_core::patch); this verb only aims the
    /// result, so whatever produced it — built-in hunks, an extension's
    /// selection, a patch pasted by hand — goes through the same door.
    ///
    /// An empty patch is refused here rather than spawned: git would answer
    /// it with a usage error that says nothing about why nothing happened.
    /// A patch whose context has drifted — the usual case being an index
    /// that moved since the diff was drawn — fails with git's own sentence,
    /// verbatim, because "patch does not apply" is advice only the person
    /// holding both sides can act on.
    ///
    /// A patch of pure additions — staging a brand-new file hunk-wise — is
    /// not served: `git apply --cached` creates the entry only from a patch
    /// carrying its file mode, and the line model does not carry one. The
    /// caller that knows it is looking at an untracked file refuses before
    /// here; the backend's own answer to such a patch is git's, and it says
    /// the missing side plainly. Whole-file staging of untracked work stays
    /// with [`stage`](Self::stage), which takes the mode from disk.
    fn stage_patch(&self, _patch: &[u8]) -> Result<()> {
        Err(unserved("patch staging"))
    }

    /// Removes exactly what `patch` describes from the index:
    /// `git apply --cached --reverse`.
    ///
    /// Unstaging at hunk granularity is the same text run backwards against
    /// the same target — the index — so it shares
    /// [`stage_patch`](Self::stage_patch)'s transport, refusals and honesty
    /// about drifted context. The working tree is never touched.
    fn unstage_patch(&self, _patch: &[u8]) -> Result<()> {
        Err(unserved("patch unstaging"))
    }

    /// Removes exactly what `patch` describes from the working tree:
    /// `git apply --reverse` without `--cached`.
    ///
    /// DESTRUCTIVE — the discarded lines end nowhere recoverable unless a
    /// copy sits staged or committed elsewhere, which is why callers confirm
    /// before this job is ever built. The index is not touched, so work
    /// staged earlier survives: discarding aims at the working tree and says
    /// so, the same line [`discard`](Self::discard) holds for whole files.
    fn discard_patch(&self, _patch: &[u8]) -> Result<()> {
        Err(unserved("patch discarding"))
    }

    /// Commits what the index holds with `message`, returning the new
    /// commit's OID.
    ///
    /// Hooks run, untouched — that is WHY this shells out rather than writing
    /// an object through a library. An empty or whitespace-only message is
    /// refused here, because git's own answer to one ("nothing to commit")
    /// names the wrong failure.
    fn commit(&self, _message: &str) -> Result<String> {
        Err(unserved("committing"))
    }

    /// Moves HEAD onto the named local branch: `git checkout -q`.
    ///
    /// The name travels as bytes and lands in argv as bytes — the same
    /// discipline as every verb above, so the branch checked out is the one
    /// [`branches`](Self::branches) named. A working tree whose changes would
    /// be lost is git's refusal, not ours: its own sentence comes back
    /// verbatim, because "commit your changes or stash them" is advice only
    /// the person reading it can act on.
    ///
    /// Deliberately **not** spelled behind `--`. Everything after that
    /// separator is a *pathspec*, and `git checkout -q -- main` would quietly
    /// restore paths matching `main` instead of moving HEAD — the one way
    /// this verb could run while doing nothing it said. What makes the bare
    /// form safe instead is a check here, not a property of refnames: git's
    /// *porcelain* refuses to create refs beginning with `-`, but its
    /// plumbing does not (`git update-ref refs/heads/--detach HEAD`
    /// succeeds on any repository), and a name like that handed to argv
    /// arrives as a **flag** — `git checkout -q --detach` detaches HEAD
    /// rather than checking out a branch somebody spelled. Names beginning
    /// with `-` are refused before the process runs; see [`refuse_dashes`].
    fn checkout(&self, _name: &[u8]) -> Result<()> {
        Err(unserved("checkout"))
    }

    /// Creates a local branch at `start`, or at HEAD when `start` is `None`.
    ///
    /// Creating never checks anything out — HEAD stays where it was, which is
    /// what makes this safe to offer beside [`checkout`](Self::checkout)
    /// without a confirmation dance between them. Only emptiness is refused
    /// here (`git branch ""` answers "not a valid branch name", which is
    /// true but says nothing a panel can repeat); every other rule of
    /// ref spelling is git's, and its error comes back quoted.
    fn create_branch(&self, _name: &[u8], _start: Option<&[u8]>) -> Result<()> {
        Err(unserved("branch creation"))
    }

    /// Deletes a local branch — merged work only, unless `force`.
    ///
    /// `-d` is git's own safety: a branch holding commits nowhere else is
    /// refused in words ("not fully merged") that come back verbatim, and
    /// `force` is the reader's explicit answer to exactly that sentence.
    /// Deleting the checked-out branch is likewise git's refusal, taken as
    /// the truth rather than re-implemented ahead of the call.
    fn delete_branch(&self, _name: &[u8], _force: bool) -> Result<()> {
        Err(unserved("branch deletion"))
    }

    /// Renames a local branch.
    ///
    /// Git's `-m` moves the ref, its configuration and its upstream link
    /// together, which is several facts a hand-rolled delete-plus-create
    /// would drop. The new name gets the same emptiness check as
    /// [`create_branch`](Self::create_branch); everything else is git's.
    fn rename_branch(&self, _from: &[u8], _to: &[u8]) -> Result<()> {
        Err(unserved("branch rename"))
    }

    /// Parks the tracked changes of the working tree on the stash stack —
    /// `git stash push` — returning the new entry's index, which is always
    /// `0`: a push puts its entry at the top.
    ///
    /// `message` names the entry; without one git writes its own `WIP on …`.
    /// Untracked files are deliberately out tonight: plain `push`, no `-u`,
    /// because parking *tracked* work is the verb the files pane asks for and
    /// `-u` quietly widens what a keypress takes away. It arrives when
    /// something asks for it by name.
    fn stash_push(&self, _message: Option<&str>) -> Result<usize> {
        Err(unserved("stashing"))
    }

    /// Restores stash `index` into the working tree, keeping the entry on the
    /// stack.
    fn stash_apply(&self, _index: usize) -> Result<()> {
        Err(unserved("applying a stash"))
    }

    /// Restores stash `index` into the working tree and drops the entry —
    /// but only when the restore was clean, which git itself decides: on a
    /// conflict it refuses, keeps the entry and says so, and those are its
    /// words surfaced verbatim, not ours. Nothing here drops as a separate
    /// step, so nothing here can lose a stash whose apply half failed.
    fn stash_pop(&self, _index: usize) -> Result<()> {
        Err(unserved("popping a stash"))
    }

    /// Deletes stash `index` off the stack. Destructive and final — the
    /// entry's commits survive in the object database until they age out,
    /// but no ref names them any more. Every index after it shifts down one,
    /// which is why callers re-read [`Self::stashes`] rather than holding
    /// positions across any of these verbs.
    fn stash_drop(&self, _index: usize) -> Result<()> {
        Err(unserved("dropping a stash"))
    }

    /// Moves the branch HEAD names onto `target` — `git reset -q --<mode>`.
    ///
    /// The mode says how much follows the pointer: [`ResetMode::Soft`] moves
    /// the branch alone, [`ResetMode::Mixed`] takes the index with it, and
    /// [`ResetMode::Hard`] sweeps the working tree too, which is why that one
    /// strength is the caller's to confirm before this is ever reached — the
    /// trait runs what it was asked for and asks nothing itself. Soft and
    /// mixed destroy nothing: every abandoned commit stays reachable through
    /// the reflog.
    ///
    /// `target` is a revspec in git's own language — a sha from
    /// [`log`](Self::log), `HEAD~1`, a branch name — and rides argv guarded
    /// by [`refuse_dashes`] like every name-shaped word here, because a rev
    /// never begins with `-` any more than a refname does.
    fn reset(&self, _mode: ResetMode, _target: &[u8]) -> Result<()> {
        Err(unserved("resetting"))
    }

    /// Undoes one commit by applying its inverse — `git revert --no-edit`.
    ///
    /// History grows; nothing moves. A new commit lands on HEAD carrying the
    /// opposite of `commit`'s change, so this needs no confirmation anywhere:
    /// dropping the result undoes the undo. A commit whose inverse does not
    /// apply cleanly (it touches lines later commits rewrote) refuses, leaves
    /// the conflict in the working tree where a human resolves it, and its
    /// refusal comes back verbatim — "your local changes would be
    /// overwritten" or git's conflict summary, never a paraphrase, because
    /// which paths conflicted is the useful half of the sentence.
    fn revert(&self, _commit: &[u8]) -> Result<()> {
        Err(unserved("reverting"))
    }

    /// Rewrites HEAD to hold the same tree plus whatever the index has now,
    /// under `message`, returning the replacement commit's OID.
    ///
    /// `git commit --amend` over the same stdin transport
    /// [`commit`](Self::commit) uses, so hooks run exactly as they would for
    /// a fresh commit and the message arrives byte-for-byte. An empty
    /// message is refused on the same terms as [`commit`](Self::commit); an
    /// unborn branch is refused *before any process runs*, because there is
    /// nothing yet to amend and git's own answer to it ("bad default
    /// revision") names nothing a person can act on.
    ///
    /// Amending a commit some remote already holds rewrites shared history —
    /// the next push will refuse it until forced. Nothing here tracks push
    /// state, so that decision stays entirely the caller's tonight; the
    /// guard arrives when the ahead/behind read can be asked honestly.
    fn amend(&self, _message: &str) -> Result<String> {
        Err(unserved("amending"))
    }

    /// Rewrites history by handing git a plan: `git rebase -i <upstream>`
    /// with the sequencer editor replaced by a command that installs
    /// [`script`](gitten_core::rebase::TodoScript).
    ///
    /// Interactive rebase is git's own machinery for rewriting a stretch of
    /// history — reorder, squash, fixup, drop, run shell commands between
    /// picks — and the *only* honest way to drive it is to let git keep that
    /// machinery and script the one human moment in it. Git generates its
    /// plan into `.git/rebase-merge/git-rebase-todo` and opens it with
    /// `$GIT_SEQUENCE_EDITOR <todo>`; this verb points that variable at
    /// `cp <our file>`, which overwrites git's plan with ours and exits 0.
    /// No editor runs, no reentry happens, hooks and `.gitconfig` behave as
    /// they would at a terminal — because it is one. (`GIT_EDITOR` is set to
    /// `true`: a `squash` opens it on a message git has already assembled,
    /// and accepting that text is what keeps git's own concatenation rule
    /// while nothing waits on a prompt.)
    ///
    /// The script travels through [`gitten_core::rebase`], which refuses
    /// anything that would need a *human* editor mid-rebase (`reword`,
    /// `edit`) before any process runs; a hung background job waiting on an
    /// invisible prompt is the failure this ordering exists to prevent.
    ///
    /// No autostash, deliberately: a dirty tree is git's refusal ("you have
    /// unstaged changes"), surfaced verbatim, because stashing work behind a
    /// keypress that said *rebase* hides exactly the state the reader should
    /// decide about. A conflict mid-rewrite exits nonzero with
    /// `.git/rebase-merge/` left standing — also verbatim — and the tree in
    /// whatever state git stopped it at; [`rebase_in_progress`](Self::rebase_in_progress)
    /// detects that state and [`rebase_abort`](Self::rebase_abort) undoes it,
    /// which keeps the human in charge of the one part of a rebase no client
    /// should automate.
    fn rebase_todo(&self, _upstream: &[u8], _script: &TodoScript) -> Result<()> {
        Err(unserved("interactive rebase"))
    }

    /// Moves the current branch onto `upstream`, replaying its own commits:
    /// plain `git rebase -q <upstream>`, no plan involved.
    ///
    /// The non-interactive sibling of [`rebase_todo`](Self::rebase_todo) on
    /// purpose — same refusals (a dirty tree is git's sentence), same
    /// conflict story (nonzero exit, state left standing, the human drives),
    /// no force anywhere: commits already pushed come back refused when the
    /// upstream has them, which is git deciding rather than us.
    fn rebase_onto(&self, _upstream: &[u8]) -> Result<()> {
        Err(unserved("rebasing"))
    }

    /// Abandons an in-progress rebase and puts everything back:
    /// `git rebase --abort`. The branch, index and working tree return to
    /// where they were when it started — git's own guarantee, not ours.
    fn rebase_abort(&self) -> Result<()> {
        Err(unserved("aborting a rebase"))
    }

    /// Continues an in-progress rebase after a human has resolved whatever
    /// stopped it: `git rebase --continue`. Both editors are answered with
    /// `true` — the sequencer todo stands as git wrote it and commit messages
    /// keep their generated text — because continuing from a client means
    /// "carry on with what is here", never "open another window".
    fn rebase_continue(&self) -> Result<()> {
        Err(unserved("continuing a rebase"))
    }

    /// Whether a rebase is mid-flight right now, read from the repository's
    /// own state directories (`rebase-merge` / `rebase-apply`, resolved
    /// through `--git-path` so linked worktrees answer for themselves).
    ///
    /// An implementation that cannot see it answers `false` — the same
    /// posture as a read answering an empty list — and every verb here asks
    /// before it starts rather than trusting a stale answer.
    fn rebase_in_progress(&self) -> bool {
        false
    }

    /// Applies one commit onto the current branch as a new commit —
    /// `git cherry-pick <sha>`.
    ///
    /// History grows and nothing existing moves, which is why this takes no
    /// confirmation anywhere: dropping the copy undoes the pick, and the
    /// original stays exactly where it was. A conflict refuses with git's own
    /// summary verbatim and leaves its question standing in the working tree
    /// — unmerged paths in the index, `CHERRY_PICK_HEAD` on disk — which
    /// [`cherry_pick_in_progress`](Self::cherry_pick_in_progress) finds and
    /// [`cherry_pick_abort`](Self::cherry_pick_abort) undoes or
    /// [`cherry_pick_continue`](Self::cherry_pick_continue) drives onward.
    /// A pick already mid-flight refuses before any process runs: one index,
    /// one sequencer, and git's own answer to a second start names a state
    /// rather than the reason.
    fn cherry_pick(&self, _sha: &[u8]) -> Result<()> {
        Err(unserved("cherry-picking"))
    }

    /// Abandons an in-progress cherry-pick and puts everything back:
    /// `git cherry-pick --abort`. The branch, index and working tree return
    /// to where they were when the pick started — git's own guarantee, not
    /// ours.
    fn cherry_pick_abort(&self) -> Result<()> {
        Err(unserved("aborting a cherry-pick"))
    }

    /// Continues an in-progress cherry-pick after a human has resolved
    /// whatever stopped it: `git cherry-pick --continue`, with the commit
    /// message editor answered by `true` — continuing from a client means
    /// "carry on with what is here", never "open another window". A further
    /// conflict comes back refused verbatim with the state still standing.
    fn cherry_pick_continue(&self) -> Result<()> {
        Err(unserved("continuing a cherry-pick"))
    }

    /// Whether a cherry-pick is mid-flight right now, read from the
    /// repository's own sequencing state — `CHERRY_PICK_HEAD` for a single
    /// stopped pick, the `sequencer` directory for a ranged one — resolved
    /// through `--git-path` so linked worktrees answer for themselves.
    ///
    /// The same posture as [`rebase_in_progress`](Self::rebase_in_progress):
    /// an implementation that cannot see it answers `false`, and every verb
    /// here asks before it starts rather than trusting a stale answer.
    fn cherry_pick_in_progress(&self) -> bool {
        false
    }

    /// Names `target` with a tag: annotated (`-a`) carrying `message` when
    /// one is given, lightweight otherwise.
    ///
    /// An annotated tag's message rides **stdin** (`--file=-`), never argv —
    /// prose travels byte-for-byte over the same transport
    /// [`commit`](Self::commit) uses. Emptiness of the name is refused here,
    /// because git's answer to it ("not a valid tag name") says nothing a
    /// field that just closed can act on; dashes are refused before any
    /// process runs like every name-shaped word; everything else — a
    /// duplicate above all — is git's sentence, quoted verbatim, because
    /// "tag 'v1' already exists" is advice the reader acts on.
    fn create_tag(&self, _name: &[u8], _target: &[u8], _message: Option<&str>) -> Result<()> {
        Err(unserved("tag creation"))
    }

    /// Deletes one tag off `name`.
    ///
    /// Lighter than it looks: a tag is a name and not a home, so every
    /// commit it pointed at survives untouched. Nothing built-in aims this
    /// tonight — no tags pane exists yet to aim it from — so the method sits
    /// here defaulted for the pane that asks for it by name (a future wave)
    /// and for any extension that reaches the same trait first.
    #[allow(dead_code)]
    fn delete_tag(&self, _name: &[u8]) -> Result<()> {
        Err(unserved("tag deletion"))
    }

    /// Sends `branch` to `remote`: `git push -q`, plus `--set-upstream`
    /// exactly when the branch tracks nothing yet.
    ///
    /// The upstream decision is *read*, not remembered:
    /// [`branches`](Self::branches) answers whether this branch is configured
    /// against a remote-tracking ref today, and only absence adds the flag —
    /// a branch that already tracks one pushes bare, because rewriting
    /// somebody's tracking configuration is not what "push" said.
    ///
    /// Deliberately **no force tonight**, as no flag anywhere in the call: a
    /// refused non-fast-forward comes back verbatim in git's own words,
    /// because the history it would discard is the reader's to decide about,
    /// and a key that says only *push* must not decide it for them. Force
    /// arrives when something asks for it by name, like stash's `-u` did.
    ///
    /// Both names are bytes and both are guarded by [`refuse_dashes`] before
    /// any process runs — a remote spelled `--upload-pack=…` is exactly the
    /// accident that guard exists for.
    fn push(&self, _remote: &[u8], _branch: &[u8]) -> Result<()> {
        Err(unserved("pushing"))
    }

    /// Fast-forwards the current branch onto its upstream:
    /// `git pull --ff-only`.
    ///
    /// No arguments on purpose — which branch pulls from where is the
    /// repository's own configuration, so every way that can be missing is
    /// git's to refuse and its sentence comes back verbatim: no branch under
    /// HEAD ("not currently on a branch"), no tracking pair ("no tracking
    /// information"), a divergence (`--ff-only`'s whole point). The tree is
    /// untouched behind every one of those refusals, which a test holds this
    /// to. No auto-rebase hides here either: untangling history is a
    /// deliberate act, never a side effect of a sync key.
    fn pull(&self) -> Result<()> {
        Err(unserved("pulling"))
    }

    /// Updates remote-tracking refs — the one remote named, or every remote
    /// this repository knows when `None`. Nothing else moves: a fetch never
    /// touches local branches, HEAD or the working tree, which is what makes
    /// it safe behind a single unconfirmed key.
    fn fetch(&self, _remote: Option<&[u8]>) -> Result<()> {
        Err(unserved("fetching"))
    }

    /// A short label for the window title.
    ///
    /// Infallible: a repository whose branch cannot be read still has a name.
    fn describe(&self) -> String;
}

/// The error a read answers when an implementation does not serve it.
///
/// The new reads default rather than being required, so an implementation
/// serves *what it serves* without stubbing out the rest — the same shape
/// as `core::rows::Present`'s defaulted methods, and for the same reason:
/// a partial backend is a real thing (a gix port lands read by read; a test
/// fake stands in for one view). An empty list would be a lie — a repository
/// with no stashes and one whose backend cannot read stashes would look
/// identical — so the default is an error that names what was not served,
/// visible wherever errors are shown.
fn unserved(what: &str) -> String {
    format!("this repository does not serve {what}")
}

/// A shared handle to one opened repository.
///
/// What callers actually hold. Cheap to clone — a refcount bump — and usable
/// from any thread, which is what lets acquisition overlap its own pieces
/// internally while a client treats the whole thing as one value.
pub type Handle = Arc<dyn Repo>;

/// Opens a repository through this crate's binary-backed implementation.
///
/// Infallible on purpose: opening runs nothing, so a directory that is not a
/// repository fails at the first read, in the same words it always failed
/// there. An eager check would put another process on the road to the first
/// frame to learn something the first read says anyway.
pub fn open(root: &Path) -> Handle {
    Arc::new(Binary {
        root: root.to_path_buf(),
    })
}

/// The shipped implementation: the `git` binary.
///
/// Private on purpose. It is *an* answer to [`Repo`], not the surface; the day
/// gix takes over the reads, this type dies or shrinks and nothing outside the
/// crate notices.
struct Binary {
    root: PathBuf,
}

impl Repo for Binary {
    fn log(&self, limit: usize) -> Result<Vec<Commit>> {
        let n = limit.to_string();
        let bytes = run(
            &self.root,
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

    fn pairs(&self, revspec: &str) -> Result<Vec<Pair>> {
        // `-z` for NUL-separated paths, because a path may contain anything a
        // filesystem allows and git otherwise quotes and escapes it. `-M` so a
        // rename arrives as one file with two names instead of a delete and an
        // add of an identical blob.
        //
        // `--abbrev=64` is load-bearing and looks like a no-op: `--raw` abbreviates
        // OIDs by default, and `cat-file --batch` echoes back the *full* OID in its
        // response header, so an abbreviated request cannot be matched to its
        // answer. 64 is clamped to whatever the repository's hash length actually
        // is, which makes this right for SHA-256 repositories too.
        const RAW: [&str; 5] = ["--raw", "-z", "-M", "--abbrev=64", "--no-ext-diff"];
        let raw = if revspec.is_empty() {
            run(&self.root, &[&["diff"], &RAW[..], &["HEAD"]].concat())?
        } else if revspec.contains("..") {
            run(
                &self.root,
                &[&["diff"], &RAW[..], &["--end-of-options", revspec]].concat(),
            )?
        } else {
            // A bare revision means "what did this commit change".
            //
            // Merges included. Modern git emits no diff at all for a merge unless
            // asked — `git show --raw` prints zero records for one — so a merge
            // commit selected in the log would render as an empty diff, silently.
            // First-parent asks for the ordinary single-old/single-new records
            // this parser already handles. Nothing else reaches this parser: the
            // refusal of two-colon combined records in `parse_raw` below is
            // belt-and-braces against future or unknown shapes, not a
            // currently-reachable input. The flag needs git >= 2.31 (March 2021);
            // older gits reject it and every bare-revision open fails wholesale
            // rather than silently.
            run(
                &self.root,
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

        let changes = parse_raw(&raw);

        // `--raw` and `--porcelain` paths are relative to the repository's top
        // level, while `root` may be any subdirectory of it (the CLI default is
        // the cwd) — so every working-tree read below joins onto the top level,
        // never onto `root` itself. Object reads do not care: `-C` finds the
        // objects from anywhere inside.
        let top = top_level(&self.root);

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
        // is shared between them but this handle's root, and neither touches what
        // the other reads. The stream's errors surface first, as `cat-file`'s did
        // when both ran in sequence: a failure to start comes back before any
        // answer is read, and the first failed answer below comes back before
        // status is asked for. A panic in either is resumed rather than swallowed
        // because both calls used to be inline.
        let (blobs, loose) = if revspec.is_empty() {
            std::thread::scope(|s| {
                let loose = s.spawn(|| self.status());
                let blobs = BlobStream::start(&self.root, &wanted);
                (
                    blobs,
                    loose
                        .join()
                        .unwrap_or_else(|p| std::panic::resume_unwind(p)),
                )
            })
        } else {
            (
                BlobStream::start(&self.root, &wanted),
                Ok(Status::default()),
            )
        };
        let mut blobs = blobs?;

        let mut out = Vec::with_capacity(changes.len());
        // Untracked files first, so they read as new before the modifications —
        // `git status` lists them last and that is the wrong way round for a diff,
        // where the thing you just created is the thing you are looking for.
        // Fetching them early changed when they arrive, not where they land.
        out.extend(loose?.untracked.iter().filter_map(|e| loose_pair(e, &top)));
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
            let old = RawChange::synthetic(&c.old_mode, &c.old_oid).or(fetched_old);
            let new = RawChange::synthetic(&c.new_mode, &c.new_oid)
                .or(fetched_new)
                .or_else(|| new_side(&c.new_oid, &top, c.path.as_bytes()));
            let binary = old.as_ref().is_some_and(|b| is_binary(b))
                || new.as_ref().is_some_and(|b| is_binary(b));
            // The lossy decode happens here and only here: everything above —
            // the record, the batch alignment, the working-tree read — went
            // through the raw bytes, so what reaches a frontend is the display
            // form of the path git actually named.
            //
            // The OIDs ride along under exactly [`fetchable`]'s rule, which is
            // also how they were chosen for the request list: a side with no
            // blob behind it has no identity worth keying anything on.
            out.push(Pair {
                path: c.path.to_string_lossy().into_owned(),
                old_path: c
                    .old_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
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
                old_oid: fetchable(&c.old_mode, &c.old_oid).then(|| c.old_oid.clone()),
                new_oid: fetchable(&c.new_mode, &c.new_oid).then(|| c.new_oid.clone()),
                binary,
            });
        }
        blobs.finish()?;
        Ok(out)
    }

    fn status(&self) -> Result<Status> {
        // `--porcelain=v2` and not v1: v2 gives each side of the index/worktree
        // split its own letter, renames carrying their old name, conflicts as
        // records of their own, and a mode per column — which is what tells a
        // symlink from a submodule without statting anything. v1 folded all of
        // that into two letters and lost it.
        //
        // `-z` for NUL-separated records, for the reason `--raw -z` has it: a
        // path may contain anything a filesystem allows — spaces, quotes,
        // newlines — and git otherwise quotes and escapes it.
        //
        // `--untracked-files=all` expands an untracked directory into the files
        // inside it, which is what both a diff and a panel want; a directory is
        // not something to show a line of text for. It respects `.gitignore`,
        // and ignored files stay unrequested — `target/` alone would arrive as
        // forty thousand entries nobody reads. `!` records are still parsed if
        // git sends them, so a caller that asks git itself pays nothing extra.
        //
        // `--renames` keeps this model stable when a user's `status.renames`
        // config disables detection: callers need one rename with both paths,
        // not an unrelated deletion and addition.
        //
        // Bytes throughout: paths carry no encoding guarantee and the model
        // keeps them raw, so there is no decode here to get wrong.
        let raw = run(
            &self.root,
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
                "--renames",
            ],
        )?;
        Ok(parse_status(&raw))
    }

    fn describe(&self) -> String {
        // Canonicalised first: `file_name()` of `.` is `None`, and `.` is what every
        // client is given by default — so without this the commonest invocation of
        // all produces a label with the repository's name missing from it.
        let named = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let name = named
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let branch = run(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"])
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

    fn branches(&self) -> Result<Vec<Branch>> {
        // One process names every local branch, points at its commit and
        // reports its tracking pair. `%(upstream:remotename)` and
        // `%(upstream:remoteref)` are the two halves *as the configuration
        // holds them* — joining a refname by hand to get them would misread a
        // remote whose name contains a slash, and there is no other place the
        // pair is said unambiguously.
        //
        // `%(upstream:track)` rides along for two words only: "" (in sync)
        // and "[gone]". Both retire a process — an in-sync branch needs no
        // counts measured, and a gone one has none to measure — so the
        // rev-list below runs once per actually-diverged branch and never on
        // a plain open.
        //
        // Records are newline-separated with NUL-separated fields: no field
        // can hold either (ref names forbid both; the track values are git's
        // own documented spellings), and a line that does not split into
        // exactly [`BRANCH_ARITY`] fields is skipped whole rather than
        // guessed through, which is what keeps a warning line from poisoning
        // its neighbours.
        let raw = run(
            &self.root,
            &[
                "for-each-ref",
                &format!("--format={BRANCH_FORMAT}"),
                "refs/heads",
            ],
        )?;
        let mut out = Vec::new();
        for b in parse_branches(&raw) {
            let upstream = b.upstream.map(|u| {
                let (ahead, behind) = match u.track {
                    Track::Synced => (Some(0), Some(0)),
                    Track::Gone => (None, None),
                    Track::Diverged => self.counts(u.tracking_ref.as_bytes(), b.refname.as_bytes()),
                };
                Upstream {
                    remote: u.remote,
                    branch: PathBytes::from_bytes(short(u.upstream_ref.as_bytes(), HEADS_PREFIX)),
                    ahead,
                    behind,
                }
            });
            out.push(Branch {
                name: PathBytes::from_bytes(short(b.refname.as_bytes(), HEADS_PREFIX)),
                commit: b.commit,
                upstream,
                head: b.head,
            });
        }
        Ok(out)
    }

    fn remote_branches(&self) -> Result<Vec<RemoteBranch>> {
        // Same framing as `branches`, one process over the other half of the
        // ref namespace. The symbolic `refs/remotes/<remote>/HEAD` alias a
        // clone writes is not a branch and reads as one nowhere.
        let raw = run(
            &self.root,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)",
                "refs/remotes",
            ],
        )?;
        Ok(parse_remote_branches(&raw))
    }

    fn head(&self) -> Result<HeadState> {
        // `symbolic-ref -q` succeeds exactly when HEAD names a branch — an
        // *unborn* one included, which every fresh repository has — and fails
        // exactly when HEAD is detached. Its exit status is the state; the
        // commit under it is a second question.
        match run(&self.root, &["symbolic-ref", "-q", "HEAD"]) {
            Ok(name) => {
                // A fresh repository resolves HEAD to nothing: the branch is
                // a name and nothing else yet, and that is what `None` says.
                let commit = run(&self.root, &["rev-parse", "HEAD"])
                    .ok()
                    .map(|c| lossy(trimmed(&c)));
                Ok(HeadState::Branch {
                    name: PathBytes::from_bytes(short(trimmed(&name), HEADS_PREFIX)),
                    commit,
                })
            }
            // Detached HEAD must still resolve to a commit. Failing here is a
            // broken repository worth reporting, not a state worth inventing.
            Err(_) => {
                let commit = run(&self.root, &["rev-parse", "HEAD"])?;
                Ok(HeadState::Detached {
                    commit: lossy(trimmed(&commit)),
                })
            }
        }
    }

    fn stashes(&self) -> Result<Vec<Stash>> {
        // `-z` ends each entry with NUL instead of a newline, because a stash
        // message may contain anything a commit message may — everything but
        // NUL. Two fields per entry: the message runs to the entry's NUL,
        // newlines and all.
        let raw = run(&self.root, &["stash", "list", "-z", "--format=%H%x00%gs"])?;
        Ok(parse_stashes(&raw))
    }

    fn remotes(&self) -> Result<Vec<Remote>> {
        // Lines, not NUL records: neither a remote name nor a URL can carry a
        // raw newline (a config value cannot hold one), so nothing here needs
        // the stronger frame.
        let raw = run(&self.root, &["remote", "-v"])?;
        Ok(parse_remotes(&raw))
    }

    fn tags(&self) -> Result<Vec<Tag>> {
        // One process, and `%(*objectname)` is why: an annotated tag points
        // at a tag object which points at the commit, and peeling it here
        // costs nothing extra. A lightweight tag's own object already *is*
        // the commit, which the parser picks apart positionally.
        let raw = run(
            &self.root,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(*objectname)%00%(objectname)",
                "refs/tags",
            ],
        )?;
        Ok(parse_tags(&raw))
    }

    fn reflog(&self, limit: usize) -> Result<Vec<ReflogEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let n = limit.to_string();
        let raw = match run(
            &self.root,
            &["reflog", "show", "-n", &n, "--format=%h%x00%gd%x00%gs"],
        ) {
            Ok(raw) => raw,
            // An unborn branch has no reflog — every fresh repository — and
            // that is emptiness, not breakage. Which case this is comes from
            // the head model rather than from parsing stderr for a phrase
            // another locale spells differently: unborn says empty, anything
            // else passes the original error on.
            Err(e) => match self.head() {
                Ok(HeadState::Branch { commit: None, .. }) => return Ok(Vec::new()),
                _ => return Err(e),
            },
        };
        Ok(parse_reflog(&raw))
    }

    fn stage(&self, path: &[u8]) -> Result<()> {
        // `add --`, not bare `add`: a path may begin with `-`. Bytes through
        // [`run_bytes`] — the same discipline the reads keep, so the file
        // staged is the one status named, whatever its bytes are.
        run_bytes(&self.root, &[b"add", b"--", path]).map(|_| ())
    }

    fn unstage(&self, path: &[u8]) -> Result<()> {
        // `-q` because a quiet no-op is the answer this verb owes "nothing was
        // staged"; git exits zero there and for an unmatched path both. On an
        // unborn branch — every fresh repository, where HEAD names a branch
        // that does not exist yet — modern git resolves HEAD to the empty tree
        // and still unstages; that is tested, not assumed, and an older git's
        // failure surfaces as the error it is rather than being guessed around.
        run_bytes(&self.root, &[b"reset", b"-q", b"HEAD", b"--", path]).map(|_| ())
    }

    fn discard(&self, path: &[u8]) -> Result<()> {
        // `checkout --`, not bare `checkout`: the `--` is what stops a path
        // that begins with `-` reading as a flag, and [`run_bytes`] keeps
        // the name byte-exact — the file restored is the one status named.
        // The index is the source, so a staged version survives; see the
        // trait method for where that line sits.
        run_bytes(&self.root, &[b"checkout", b"--", path]).map(|_| ())
    }

    fn remove_untracked(&self, path: &[u8]) -> Result<()> {
        // A filesystem deletion and deliberately not a git command: nothing
        // about an untracked file lives in git's object database. Bytes
        // through [`join_raw`], or the read would stat a decoded near-miss
        // of somebody's real filename; the message decodes lossily because
        // it is human text and never aimed back at anything.
        //
        // The one verb that touches the filesystem directly is also the one
        // that fences where it may touch it. `join` treats an absolute path
        // as a replacement for the root, so the guard sits on the JOINED
        // result — the thing about to be unlinked — and `..` is refused in
        // the raw bytes, which a lexical prefix check cannot see through.
        if path.starts_with(b"/") || path.split(|b| *b == b'/').any(|c| c == b"..") {
            return Err(format!(
                "not inside this repository: {}",
                String::from_utf8_lossy(path)
            ));
        }
        let at = join_raw(&self.root, path);
        if !at.starts_with(&self.root) {
            return Err(format!(
                "not inside this repository: {}",
                String::from_utf8_lossy(path)
            ));
        }
        std::fs::remove_file(&at).map_err(|e| format!("could not delete {}: {e}", at.display()))
    }

    fn ignore(&self, path: &[u8]) -> Result<()> {
        let Some(line) = ignore_line(path) else {
            // No spelling exists, so nothing honest can be written: said,
            // where errors are shown, instead of a line that ignores
            // nothing. The name decodes lossily because it is prose in an
            // error message and never aimed back at the filesystem.
            return Err(format!(
                "gitignore matches a line at a time; a name with a line \
                 break cannot be ignored ({})",
                String::from_utf8_lossy(path)
            ));
        };
        let at = join_raw(&self.root, b".gitignore");
        // Absent and unreadable are the same answer here: an empty file to
        // append to. A .gitignore this process cannot read it can hardly
        // have written, and refusing to ignore over it would be noise.
        let mut next = std::fs::read(&at).unwrap_or_default();
        if next.split(|b| *b == b'\n').any(|existing| existing == line) {
            return Ok(());
        }
        // Whatever the file ended with, the new line starts on its own: a
        // missing final newline is the ordinary state of a hand-edited one,
        // and gluing onto it would silently extend somebody's last pattern.
        if !next.is_empty() && !next.ends_with(b"\n") {
            next.push(b'\n');
        }
        next.extend_from_slice(&line);
        next.push(b'\n');
        std::fs::write(&at, next).map_err(|e| format!("could not write {}: {e}", at.display()))
    }

    fn stage_many(&self, paths: &[&[u8]]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        self.run_chunked(&[b"add", b"--"], paths)
    }

    fn unstage_many(&self, paths: &[&[u8]]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        self.run_chunked(&[b"reset", b"-q", b"HEAD", b"--"], paths)
    }

    fn stage_patch(&self, patch: &[u8]) -> Result<()> {
        if patch.is_empty() {
            return Err("an empty patch stages nothing".into());
        }
        // `--whitespace=nowarn` because a synthesized patch's whitespace is
        // exactly what the diff showed — a warning about it would be git
        // relitigating a decision already on screen.
        run_stdin(
            &self.root,
            &[b"apply", b"--cached", b"--whitespace=nowarn", b"-"],
            patch,
        )
    }

    fn unstage_patch(&self, patch: &[u8]) -> Result<()> {
        if patch.is_empty() {
            return Err("an empty patch unstages nothing".into());
        }
        run_stdin(
            &self.root,
            &[
                b"apply",
                b"--cached",
                b"--reverse",
                b"--whitespace=nowarn",
                b"-",
            ],
            patch,
        )
    }

    fn discard_patch(&self, patch: &[u8]) -> Result<()> {
        if patch.is_empty() {
            return Err("an empty patch discards nothing".into());
        }
        // The worktree half of the same text: `--reverse` with no
        // `--cached`, so the index stands still while the working tree gives
        // the hunk's lines back. See the trait method for where destruction
        // is confirmed.
        run_stdin(
            &self.root,
            &[b"apply", b"--reverse", b"--whitespace=nowarn", b"-"],
            patch,
        )
    }

    fn checkout(&self, name: &[u8]) -> Result<()> {
        // `-q` keeps git's "Switched to branch" off our error band's road;
        // the name rides bare — see the trait method for why no `--` sits in
        // front of it, and [`refuse_dashes`] for what does stand guard.
        refuse_dashes(name)?;
        run_bytes(&self.root, &[b"checkout", b"-q", name]).map(|_| ())
    }

    fn create_branch(&self, name: &[u8], start: Option<&[u8]>) -> Result<()> {
        if !nameable(name) {
            return Err("a branch needs a name".into());
        }
        refuse_dashes(name)?;
        // The start point is a revspec and rides argv too; a rev never
        // begins with `-` any more than a refname does.
        if let Some(start) = start {
            refuse_dashes(start)?;
        }
        match start {
            Some(start) => run_bytes(&self.root, &[b"branch", name, start]),
            None => run_bytes(&self.root, &[b"branch", name]),
        }
        .map(|_| ())
    }

    fn delete_branch(&self, name: &[u8], force: bool) -> Result<()> {
        let flag: &[u8] = match force {
            true => b"-D",
            false => b"-d",
        };
        refuse_dashes(name)?;
        run_bytes(&self.root, &[b"branch", flag, name]).map(|_| ())
    }

    fn rename_branch(&self, from: &[u8], to: &[u8]) -> Result<()> {
        if !nameable(to) {
            return Err("a branch needs a name".into());
        }
        refuse_dashes(from)?;
        refuse_dashes(to)?;
        run_bytes(&self.root, &[b"branch", b"-m", from, to]).map(|_| ())
    }

    fn commit(&self, message: &str) -> Result<String> {
        self.commit_via(&[b"commit", b"--file=-"], message)
    }

    fn stash_push(&self, message: Option<&str>) -> Result<usize> {
        // What `stash@{0}` resolves to before the push. Git answers "nothing
        // to stash" with exit 0 and one localized sentence on stdout — no
        // flag makes it machine-readable — so the only honest test of whether
        // a stash was created is whether the top of the stack moved. Two
        // cheap rev-parses around a rare write, and no prose parsing to go
        // wrong in another locale.
        let before = self.stash_head();
        match message {
            Some(m) => run(&self.root, &["stash", "push", "-m", m])?,
            None => run(&self.root, &["stash", "push"])?,
        };
        if self.stash_head() == before {
            return Err("nothing to stash: the working tree has no tracked changes".into());
        }
        Ok(0)
    }

    fn stash_apply(&self, index: usize) -> Result<()> {
        run_bytes(
            &self.root,
            &[b"stash", b"apply", stash_ref(index).as_slice()],
        )
        .map(|_| ())
    }

    fn stash_pop(&self, index: usize) -> Result<()> {
        run_bytes(&self.root, &[b"stash", b"pop", stash_ref(index).as_slice()]).map(|_| ())
    }

    fn stash_drop(&self, index: usize) -> Result<()> {
        run_bytes(
            &self.root,
            &[b"stash", b"drop", stash_ref(index).as_slice()],
        )
        .map(|_| ())
    }

    fn reset(&self, mode: ResetMode, target: &[u8]) -> Result<()> {
        // Mixed is git's default when the flag is left off; spelled anyway,
        // for the same reason an argument-less fetch is `--all` — what the
        // verb means is said, never inherited from a default that could
        // drift. `-q` keeps git's position summary off the error band's road.
        refuse_dashes(target)?;
        // The flag comes from [`ResetMode::flag`] itself — the spelling
        // lives in `core` beside the type, and this is not a second table
        // to keep in step with it.
        run_bytes(
            &self.root,
            &[b"reset", b"-q", mode.flag().as_bytes(), target],
        )
        .map(|_| ())
    }

    fn revert(&self, commit: &[u8]) -> Result<()> {
        // `--no-edit` because the inverse commit gets git's own summary of
        // what it undoes — composing a message is a prompt this verb has no
        // field for, and "Revert \"<original subject>\"" says more than an
        // empty one would. A conflict refuses below with git's own words and
        // leaves its question in the working tree.
        refuse_dashes(commit)?;
        run_bytes(&self.root, &[b"revert", b"--no-edit", commit]).map(|_| ())
    }

    fn amend(&self, message: &str) -> Result<String> {
        // The same two refusals commit makes, said before anything runs: an
        // empty message names the wrong failure only after a process, and an
        // unborn branch has nothing to rewrite — git's answer there ("fatal:
        // bad default revision" or worse) describes no state the reader
        // recognises.
        if message.trim().is_empty() {
            return Err("a commit needs a message".into());
        }
        if let HeadState::Branch { commit: None, .. } = self.head()? {
            return Err("nothing to amend: this branch has no commits yet".into());
        }
        self.commit_via(&[b"commit", b"--amend", b"-q", b"--file=-"], message)
    }

    fn rebase_todo(&self, upstream: &[u8], script: &TodoScript) -> Result<()> {
        // The plan is checked before anything runs: a refusal that names the
        // action beats a background job hung on an editor nobody can see.
        script.validate()?;
        if self.rebase_in_progress() {
            return Err(
                "a rebase is already in progress; finish or abort it before \
                 starting another"
                    .into(),
            );
        }
        refuse_dashes(upstream)?;
        let todo = write_todo_tmpfile(script.emit())?;
        // git runs the sequencer editor as `$EDITOR <todo>`, through the
        // shell. `cp <ours>` takes the todo path as its second argument,
        // overwrites it with our plan and exits 0 — an editor that always
        // agrees with us. The temp path rides as bytes: a `$TMPDIR` with an
        // odd byte in it is unusual, not impossible.
        //
        // `GIT_EDITOR=true` answers the *second* editor: a `squash` opens it
        // on a message template git already filled in, and `true` accepts
        // that text untouched — which is precisely what keeps git's own
        // message-concatenation rule while nothing blocks on a prompt.
        let editor = {
            use std::os::unix::ffi::OsStrExt;
            format!("cp {}", shell_quote(todo.as_os_str().as_bytes()))
        };
        let result = run_env(
            &self.root,
            &[b"rebase", b"-i", upstream],
            &[("GIT_SEQUENCE_EDITOR", &editor[..]), ("GIT_EDITOR", "true")],
        );
        let _ = std::fs::remove_file(&todo);
        result.map(|_| ())
    }

    fn rebase_onto(&self, upstream: &[u8]) -> Result<()> {
        refuse_dashes(upstream)?;
        run_bytes(&self.root, &[b"rebase", b"-q", upstream]).map(|_| ())
    }

    fn rebase_abort(&self) -> Result<()> {
        run_env(
            &self.root,
            &[b"rebase", b"--abort"],
            &[("GIT_SEQUENCE_EDITOR", "true")],
        )
        .map(|_| ())
    }

    fn rebase_continue(&self) -> Result<()> {
        run_env(
            &self.root,
            &[b"rebase", b"--continue"],
            &[("GIT_SEQUENCE_EDITOR", "true"), ("GIT_EDITOR", "true")],
        )
        .map(|_| ())
    }

    fn rebase_in_progress(&self) -> bool {
        ["rebase-merge", "rebase-apply"]
            .iter()
            .any(|state| self.git_state_exists(state))
    }

    fn cherry_pick(&self, sha: &[u8]) -> Result<()> {
        // One index and one sequencer: a second start cannot share them with
        // the first, and refusing before any process runs says so in words
        // that name the way out — finish or abort — where git's own answer
        // to a second start arrives only after disturbing the first.
        if self.cherry_pick_in_progress() {
            return Err("a cherry-pick is already in progress; finish or abort it \
                 before starting another"
                .into());
        }
        // The sha is a revspec and rides argv like every name-shaped word;
        // see [`refuse_dashes`] for what stands guard.
        refuse_dashes(sha)?;
        run_bytes(&self.root, &[b"cherry-pick", sha]).map(|_| ())
    }

    fn cherry_pick_abort(&self) -> Result<()> {
        run_bytes(&self.root, &[b"cherry-pick", b"--abort"]).map(|_| ())
    }

    fn cherry_pick_continue(&self) -> Result<()> {
        run_env(
            &self.root,
            &[b"cherry-pick", b"--continue"],
            &[("GIT_EDITOR", "true")],
        )
        .map(|_| ())
    }

    fn cherry_pick_in_progress(&self) -> bool {
        ["CHERRY_PICK_HEAD", "sequencer"]
            .iter()
            .any(|state| self.git_state_exists(state))
    }

    fn create_tag(&self, name: &[u8], target: &[u8], message: Option<&str>) -> Result<()> {
        if !nameable(name) {
            return Err("a tag needs a name".into());
        }
        refuse_dashes(name)?;
        // The target is a revspec and rides argv too; a rev never begins
        // with `-` any more than a refname does.
        refuse_dashes(target)?;
        match message {
            // `-a --file=-` is `-m`'s stdin spelling: same annotated tag,
            // prose byte-for-byte instead of an argv-escaping exercise.
            Some(message) => run_stdin(
                &self.root,
                &[b"tag", b"-a", b"--file=-", name, target],
                message.as_bytes(),
            ),
            None => run_bytes(&self.root, &[b"tag", name, target]).map(|_| ()),
        }
    }

    fn delete_tag(&self, name: &[u8]) -> Result<()> {
        refuse_dashes(name)?;
        run_bytes(&self.root, &[b"tag", b"-d", name]).map(|_| ())
    }

    fn push(&self, remote: &[u8], branch: &[u8]) -> Result<()> {
        refuse_dashes(remote)?;
        refuse_dashes(branch)?;
        // The flag rides only when the branches read proves absence — the
        // same read the branches panel draws its tracking pairs from, so a
        // verb and a pane cannot disagree about what tracks what. A read
        // that failed proves nothing either way, and pushing bare rewrites
        // nothing, so absence of proof means absence of the flag.
        let tracked = self
            .branches()
            .ok()
            .and_then(|all| {
                all.iter()
                    .find(|b| b.name.as_bytes() == branch)
                    .map(|b| b.upstream.is_some())
            })
            .unwrap_or(false);
        let argv: &[&[u8]] = match tracked {
            true => &[b"push", b"-q", remote, branch],
            false => &[b"push", b"-q", b"--set-upstream", remote, branch],
        };
        run_bytes(&self.root, argv).map(|_| ())
    }

    fn pull(&self) -> Result<()> {
        // Everything specific — which branch, which upstream, what a
        // divergence means — is git's to resolve from the repository's own
        // configuration, which is also why there is no name here to guard:
        // nothing we chose rides argv. `--ff-only` is the one word added,
        // because moving a branch sideways without being asked is exactly
        // what a client must never do quietly; git's refusal to move at all
        // is surfaced verbatim by [`run_bytes`], tree intact.
        run_bytes(&self.root, &[b"pull", b"--ff-only"]).map(|_| ())
    }

    fn fetch(&self, remote: Option<&[u8]>) -> Result<()> {
        match remote {
            Some(remote) => {
                refuse_dashes(remote)?;
                run_bytes(&self.root, &[b"fetch", b"-q", remote]).map(|_| ())
            }
            // Spelled `--all` rather than left off: what an argument-less
            // fetch covers has drifted across git versions and depends on
            // config besides, and "every remote this repository knows"
            // should be said, not inherited.
            None => run_bytes(&self.root, &[b"fetch", b"-q", b"--all"]).map(|_| ()),
        }
    }
}

impl Binary {
    /// Runs `head ++ paths` through [`run_bytes`], in as many processes as
    /// [`ARGV_BUDGET`] demands — one for every list a person actually
    /// stages, several only for the fresh-repository trees bulk exists for.
    fn run_chunked(&self, head: &[&[u8]], paths: &[&[u8]]) -> Result<()> {
        let mut at = 0;
        while at < paths.len() {
            let end = chunk_end(paths, at);
            let mut args: Vec<&[u8]> = head.to_vec();
            args.extend(paths[at..end].iter().copied());
            run_bytes(&self.root, &args).map(|_| ())?;
            at = end;
        }
        Ok(())
    }

    /// One commit-shaped write — a fresh one or an amend — with `message`
    /// riding stdin under whatever argv `head` spells.
    ///
    /// The message rides stdin (`--file=-`), never an argv word: quotes,
    /// newlines and non-ASCII arrive byte-for-byte instead of surviving an
    /// escaping exercise here. Hooks see exactly what they would from a
    /// terminal commit. The returned OID is asked for afterwards rather than
    /// parsed out of git's prose: hook output shares the stream with git's
    /// own summary, and the answer is one cheap rev-parse away.
    fn commit_via(&self, head: &[&[u8]], message: &str) -> Result<String> {
        use std::os::unix::ffi::OsStrExt;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(head.iter().map(|a| std::ffi::OsStr::from_bytes(a)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run git commit: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        // A write failure (a hook that quit before reading) is git's story to
        // tell through the exit status; ours would only be noise in front of it.
        let _ = stdin.write_all(message.as_bytes());
        drop(stdin); // EOF is the message's end
        let out = child
            .wait_with_output()
            .map_err(|e| format!("git commit: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git commit: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let sha = run(&self.root, &["rev-parse", "HEAD"])?;
        Ok(lossy(trimmed(&sha)))
    }

    /// What `stash@{0}` resolves to right now, when a stack exists at all.
    ///
    /// The question [`Repo::stash_push`] asks twice — around the push — and
    /// nothing else asks. `-q --verify` so an empty stack is `None` rather
    /// than a stderr of its own.
    fn stash_head(&self) -> Option<String> {
        run(&self.root, &["rev-parse", "-q", "--verify", "stash@{0}"])
            .ok()
            .map(|oid| lossy(trimmed(&oid)))
    }

    /// Whether one of git's sequencing state directories exists —
    /// `rebase-merge` for the modern interactive rebase, `rebase-apply` for
    /// `am`-shaped ones and older git's fallback. Resolved through
    /// `--git-path`, which is what makes a linked worktree answer about its
    /// own state directory instead of the main `.git`.
    fn git_state_exists(&self, name: &str) -> bool {
        let Ok(raw) = run(&self.root, &["rev-parse", "--git-path", name]) else {
            return false;
        };
        let shown = trimmed(&raw);
        if shown.is_empty() {
            return false;
        }
        use std::os::unix::ffi::OsStrExt;
        let at = Path::new(std::ffi::OsStr::from_bytes(shown));
        match at.is_absolute() {
            true => at.exists(),
            // Relative answers are relative to where git was pointed.
            false => self.root.join(at).exists(),
        }
    }

    /// Ahead and behind between a branch and its upstream: one process, both
    /// numbers.
    ///
    /// The symmetric difference is written upstream-first — `upstream...local`,
    /// the direction the question is asked from ("what would a pull bring?")
    /// — so `--left-right` counts the *left* side as behind and the right as
    /// ahead. The swap happens here, at the only place the argument order is
    /// knowable, and never downstream.
    ///
    /// Bytes end to end: these names address git, and handing over their
    /// lossy spelling would count a different branch's commits.
    fn counts(&self, upstream: &[u8], local: &[u8]) -> (Option<u32>, Option<u32>) {
        let mut range = Vec::with_capacity(upstream.len() + 3 + local.len());
        range.extend_from_slice(upstream);
        range.extend_from_slice(b"...");
        range.extend_from_slice(local);
        let raw = match run_bytes(
            &self.root,
            &[b"rev-list", b"--left-right", b"--count", &range],
        ) {
            Ok(raw) => raw,
            // One unreadable branch must not take the whole answer down:
            // a rebase moving the upstream mid-read, or a shallow clone
            // missing one side, fails this one call — and the model already
            // has a word, `None`, for "cannot be compared".
            Err(_) => return (None, None),
        };
        let text = String::from_utf8_lossy(&raw);
        let mut sides = text.split_whitespace();
        match (
            sides.next().and_then(|s| s.parse().ok()),
            sides.next().and_then(|s| s.parse().ok()),
        ) {
            (Some(behind), Some(ahead)) => (Some(ahead), Some(behind)),
            _ => (None, None),
        }
    }
}

/// The `.gitignore` spelling of one path: anchored at the repository root,
/// with every character git would otherwise read as pattern syntax made
/// literal — or [`None`] for a name no `.gitignore` line can match.
///
/// Three layers, because `.gitignore` is a pattern language and a filename
/// is not:
///
/// - **A leading `/`** pins the name to the root — without it, `log.txt`
///   would match at any depth and hide files nobody meant to ignore. It also
///   takes `#` and `!` out of their special first positions for free.
/// - **Glob characters** (`*`, `?`, `[`, `]`) and the name's own backslashes
///   are backslash-escaped wherever they occur; `weird[name].txt` is a name,
///   not a class. Quotes and tabs need nothing — mid-line they are ordinary
///   bytes to git, checked against the binary rather than assumed.
/// - **Trailing spaces** are trimmed from every line unless escaped, so each
///   one rides behind a backslash.
///
/// What has no spelling is a line break: patterns are read one line at a
/// time, so a name holding a newline can be matched by nothing this file can
/// hold — checked, not argued. `None` there is what keeps the verb from
/// writing a line that looks done and ignores nothing.
fn ignore_line(path: &[u8]) -> Option<Vec<u8>> {
    if path.iter().any(|b| matches!(b, b'\n' | b'\r')) {
        return None;
    }
    let mut body = Vec::with_capacity(path.len() + 1);
    body.push(b'/');
    for &b in path {
        if matches!(b, b'*' | b'?' | b'[' | b']' | b'\\') {
            body.push(b'\\');
        }
        body.push(b);
    }
    // Everything from the last non-space onwards is trailing space to git;
    // escape each rather than only the run's head, which is what keeps two
    // trailing blanks both. The leading `/` is a non-space, so the position
    // below is always found.
    let keep = body.iter().rposition(|b| *b != b' ').unwrap_or(0) + 1;
    if keep < body.len() {
        let spaces = body.len() - keep;
        body.truncate(keep);
        for _ in 0..spaces {
            body.extend_from_slice(b"\\ ");
        }
    }
    Some(body)
}

/// Whether a name is worth handing to git at all.
///
/// The one rule checked before git sees it: emptiness, whitespace only or
/// not there at all. Git's own answer to `git branch ""` is "not a valid
/// branch name" — true, and useless beside a field that just closed — so
/// the refusal here says what the reader can act on instead. Every other
/// rule of ref spelling (`..`, trailing `.lock`) stays git's, because its
/// error quotes the offending name back.
fn nameable(name: &[u8]) -> bool {
    !name.is_empty() && name.iter().any(|b| !b.is_ascii_whitespace())
}

/// Refuses any name-shaped argument whose first byte is `-`.
///
/// Not paranoia about names git would never hold: plumbing holds them.
/// `git update-ref refs/heads/--detach HEAD` succeeds where
/// `git branch --detach` refuses, and the ref then sits in
/// [`branches`](Repo::branches) spelled exactly like an option. Handed back
/// through argv bare — which is how every verb here addresses a name — it
/// *is* an option: `git checkout -q --detach` detaches HEAD instead of
/// erroring, and worse spellings are one release of git away from meaning
/// something else. So the refusal is ours and it is up front, in words
/// that name the rule rather than git's usage dump; the panel still shows
/// the branch, because showing is not aiming.
fn refuse_dashes(name: &[u8]) -> Result<()> {
    if name.first() == Some(&b'-') {
        return Err(format!(
            "names beginning with '-' are refused ({})",
            String::from_utf8_lossy(name)
        ));
    }
    Ok(())
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
///
/// The two OIDs are why acquisition runs `--abbrev=64` at all: they are what
/// makes a re-diff of an unchanged file skippable, so [`Pair`] carries them
/// beside the content rather than letting them die in [`RawChange`].
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
    /// The blob the old side came from, or `None` when there is none: a null
    /// OID (`added`, `deleted`) and a gitlink have no blob in *this*
    /// repository's object database, and neither does anything read off the
    /// working tree. Exactly [`fetchable`]'s predicate — one definition of
    /// "this side is a real blob", not two.
    ///
    /// A blob's content never changes, so `(old_oid, new_oid)` names the pair
    /// of texts above completely; that is what the diff cache keys on.
    pub old_oid: Option<String>,
    /// The new side's blob, under the same rule as [`Pair::old_oid`]. An
    /// untracked file's contents live nowhere but disk, so its new side is
    /// `None` even though its text is right there in `new`.
    pub new_oid: Option<String>,
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

    /// Both OIDs, when both sides are real blobs. Anything else — an added or
    /// deleted file, a gitlink, an untracked file — is `None`, and a caller
    /// keying a cache must treat it as *always compute* rather than inventing
    /// a key from partial identity: one known OID says nothing about the text
    /// on the other side.
    pub fn blobs(&self) -> Option<(&str, &str)> {
        Some((self.old_oid.as_deref()?, self.new_oid.as_deref()?))
    }
}

///
/// The frontend never learns which implementation ran, and never learns whether
/// the content came from the object database or the working tree.
pub fn diff(
    repo: &dyn Repo,
    revspec: &str,
    differs: &Differs,
    over: &Overrides,
) -> Result<Vec<FileDiff>> {
    Ok(repo
        .pairs(revspec)?
        .iter()
        .map(|p| match p.binary {
            // Modelled as a file with no hunks rather than skipped: the diff
            // still has to say the file changed, and "binary" is the honest
            // thing for it to say.
            true => FileDiff {
                path: p.label(),
                hunks: Vec::new(),
            },
            // The OIDs go with the text, so a re-diff of an unchanged file —
            // every refresh after every unrelated write — is remembered work.
            // A pair without both OIDs computes as always; see
            // [`Differs::file_using`] for what that covers.
            false => FileDiff {
                path: p.label(),
                ..differs.file_using(over, &p.path, &p.old, &p.new, p.blobs())
            },
        })
        .collect())
}

// -------------------------------------------------------------------- status

/// `git status --porcelain=v2 -z`, parsed into the model in
/// [`gitten_core::status`](gitten_core::status).
///
/// The grammar, as git emits it under `-z`: records separated by NUL, fixed
/// fields separated by spaces, and the path — which may contain spaces,
/// quotes and newlines — ending at the record's NUL. Renames carry their old
/// name as one more NUL-delimited field after the path:
///
/// ```text
/// 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>\0
/// 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>\0<origPath>\0
/// u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>\0
/// ? <path>\0          ! <path>\0
/// ```
///
/// One ordinary record can feed **two** lists — edited, staged and edited
/// again is `1 MM`, staged *and* unstaged for the same path — and a conflicted
/// path feeds neither, because its truth lives in three index stages no single
/// X or Y could name.
///
/// Empty fields are preserved, not filtered: the field after a type-2 record
/// is its old name *by position*, and an empty one is a real answer ("no old
/// name arrived") rather than noise. Filtering empties would slide every later
/// record into that slot and swallow it.
fn parse_status(raw: &[u8]) -> Status {
    let mut out = Status::default();
    let mut records = raw.split(|b| *b == 0);
    while let Some(rec) = records.next() {
        // A trailing or doubled NUL makes an empty piece; nothing to read.
        if rec.is_empty() {
            continue;
        }
        if rec.starts_with(b"1 ") {
            ordinary(rec, &mut out);
        } else if rec.starts_with(b"2 ") {
            // The old name is the next field, taken whether this record parses
            // or not — empty included, because *that* is what an empty field
            // is for. Skipping a broken one must not leave the cursor sitting
            // on somebody else's path, or one bad record corrupts two.
            let old = records.next();
            renamed(rec, old, &mut out);
        } else if rec.starts_with(b"u ") {
            unmerged(rec, &mut out);
        } else if rec.starts_with(b"?") || rec.starts_with(b"!") {
            loose(rec, &mut out);
        }
        // Anything else — a truncated tail, a header from a flag nobody asked
        // for — is skipped rather than guessed at, the same rule `parse_raw`
        // sets for `--raw`.
    }
    out
}

/// The fixed fields of one record and the path that ends it.
///
/// Everything before the path is space-separated and none of it contains a
/// space — two-letter codes, octal modes, hex hashes, a score glued to its
/// letter — so splitting exactly `fixed` times leaves the whole path as the
/// final piece, spaces and newlines intact. Fewer fields than that is a
/// malformed record: `None`, skip it.
fn fields(rec: &[u8], fixed: usize) -> Option<(Vec<&[u8]>, &[u8])> {
    let mut parts = rec.splitn(fixed, |b| *b == b' ');
    let head: Vec<&[u8]> = (&mut parts).take(fixed - 1).collect();
    let path = parts.next()?;
    if head.len() != fixed - 1 || path.is_empty() {
        return None;
    }
    Some((head, path))
}

/// One letter of an `XY` pair: `None` when that side is unchanged, a
/// [`Change`] when it names one, and no third shape — an unrecognised letter
/// makes the record malformed rather than guessed at.
fn change(letter: u8) -> Option<Option<Change>> {
    Some(match letter {
        b'.' => None,
        b'A' => Some(Change::Added),
        b'M' => Some(Change::Modified),
        b'D' => Some(Change::Deleted),
        b'T' => Some(Change::TypeChanged),
        b'R' => Some(Change::Renamed),
        b'C' => Some(Change::Copied),
        _ => return None,
    })
}

/// Both letters of an `XY` field, or nothing when either is unreadable.
fn xy(field: &[u8]) -> Option<(Option<Change>, Option<Change>)> {
    if field.len() != 2 {
        return None;
    }
    Some((change(field[0])?, change(field[1])?))
}

/// The mode bytes as mode text, for [`Kind::from_git_mode`].
fn mode(field: &[u8]) -> Kind {
    Kind::from_git_mode(std::str::from_utf8(field).unwrap_or(""))
}

/// The mode column of a side that does not exist. Porcelain v2 prints six
/// zeros wherever a mode is called for and the thing is gone.
const ABSENT_MODE: &[u8] = b"000000";

fn exists(field: &[u8]) -> bool {
    field != ABSENT_MODE
}

/// The kind of one side of a record: from its own column's mode when that
/// side exists, else from a column that did.
///
/// A destination that does not exist prints [`ABSENT_MODE`] — nothing in the
/// index for a staged deletion, nothing in the working tree for an unstaged
/// one, nothing anywhere for a conflict resolved away — and `000000` parses
/// as a plain file, which would quietly relabel every deleted symlink and
/// every deleted submodule as something to read or stage as text. The
/// fallbacks are the columns of the sides that *did* exist, most local first:
/// what a path was is knowable from its own record even when where it ended
/// up is not. Nothing anywhere is guessed low — [`Kind::File`] — exactly as
/// [`Kind::from_git_mode`] guesses on a malformed mode.
fn kind_of(primary: &[u8], fallbacks: &[&[u8]]) -> Kind {
    if exists(primary) {
        return mode(primary);
    }
    match fallbacks.iter().find(|f| exists(f)) {
        Some(f) => mode(f),
        None => Kind::File,
    }
}

/// The submodule state field, as bytes: `N...` when the entry is not a
/// submodule, else `S<C><M><U>` with each flag a letter or a dot.
///
/// What the flags *mean* is a working-tree question and lives on
/// [`Submodule`] in `core` — including why they never ride on a staged
/// entry. Anything shorter or stranger parses as "no flags claimed" — a state
/// nobody has seen yet is not a reason to drop the entry, and [`Kind`]
/// still says whether the path is a submodule from its mode.
fn submodule_state(field: &[u8]) -> Submodule {
    if field.first() != Some(&b'S') || field.len() < 4 {
        return Submodule::default();
    }
    Submodule {
        commit_changed: field[1] == b'C',
        modified: field[2] == b'M',
        untracked: field[3] == b'U',
    }
}

/// An ordinary (`1`) record: one path, one X, one Y.
fn ordinary(rec: &[u8], out: &mut Status) {
    // tag XY sub mH mI mW hH hI path — eight fields, then the path.
    let Some((f, path)) = fields(rec, 9) else {
        return;
    };
    let Some((x, y)) = xy(f[1]) else { return };
    let sub = submodule_state(f[2]);
    if let Some(c) = x {
        out.staged.push(StagedEntry {
            path: PathBytes::from_bytes(path),
            change: c,
            old_path: None,
            kind: kind_of(f[4], &[f[3]]), // mI; a staged deletion falls back to mH
            // The submodule state field describes the working tree, and a
            // staged entry describes the index — see [`Submodule`].
            submodule: Submodule::default(),
        });
    }
    if let Some(c) = y {
        out.unstaged.push(UnstagedEntry {
            path: PathBytes::from_bytes(path),
            change: c,
            kind: kind_of(f[5], &[f[4]]), // mW; an unstaged deletion falls back to mI
            submodule: sub,
        });
    }
}

/// A rename/copy (`2`) record, whose old name arrived as its own field.
fn renamed(rec: &[u8], old: Option<&[u8]>, out: &mut Status) {
    // tag XY sub mH mI mW hH hI <X><score> path — nine fields, then the path.
    let Some((f, path)) = fields(rec, 10) else {
        return;
    };
    let Some((x, y)) = xy(f[1]) else { return };
    let sub = submodule_state(f[2]);
    // Whether an old name exists is decided by what X says, and a record
    // without its second field still shows — labelled by the new name only,
    // which is more honest than dropping a change that happened.
    let old_path = match x {
        Some(Change::Renamed | Change::Copied) => old.map(PathBytes::from_bytes),
        _ => None,
    };
    if let Some(c) = x {
        out.staged.push(StagedEntry {
            path: PathBytes::from_bytes(path),
            change: c,
            old_path,
            kind: kind_of(f[4], &[f[3]]),
            submodule: Submodule::default(),
        });
    }
    if let Some(c) = y {
        out.unstaged.push(UnstagedEntry {
            path: PathBytes::from_bytes(path),
            change: c,
            kind: kind_of(f[5], &[f[4]]),
            submodule: sub,
        });
    }
}

/// An unmerged (`u`) record: a merge left all three stages behind.
fn unmerged(rec: &[u8], out: &mut Status) {
    // tag XY sub m1 m2 m3 mW h1 h2 h3 path — ten fields, then the path.
    let Some((f, path)) = fields(rec, 11) else {
        return;
    };
    let Some(state) = conflict_kind(f[1]) else {
        return;
    };
    out.conflicts.push(ConflictEntry {
        path: PathBytes::from_bytes(path),
        state,
        // mW is what the working tree holds right now. A conflict can resolve
        // the worktree side away (a file removed mid-merge, both sides having
        // deleted it) while the stages still say what existed — fall back
        // through those, base then ours then theirs.
        kind: kind_of(f[6], &[f[3], f[4], f[5]]),
        submodule: submodule_state(f[2]),
    });
}

/// The seven disagreements a merge can leave, keyed by the XY git prints.
fn conflict_kind(xy: &[u8]) -> Option<ConflictKind> {
    Some(match xy {
        b"DD" => ConflictKind::BothDeleted,
        b"AU" => ConflictKind::AddedByUs,
        b"UD" => ConflictKind::DeletedByThem,
        b"UA" => ConflictKind::AddedByThem,
        b"DU" => ConflictKind::DeletedByUs,
        b"AA" => ConflictKind::BothAdded,
        b"UU" => ConflictKind::BothModified,
        _ => return None,
    })
}

/// A `?` untracked or `!` ignored record: tag glued to the path.
fn loose(rec: &[u8], out: &mut Status) {
    let path = &rec[1..];
    // Exactly one space separates tag from path, and only one is trimmed: the
    // path may begin with a space itself, and trimming twice renames it.
    let path = match path.strip_prefix(b" ") {
        Some(rest) => rest,
        None => path,
    };
    if path.is_empty() {
        return;
    }
    if rec[0] == b'?' {
        out.untracked.push(UntrackedEntry {
            path: PathBytes::from_bytes(path),
        });
    } else {
        out.ignored.push(PathBytes::from_bytes(path));
    }
}

/// One untracked entry as a [`Pair`] with nothing opposite it.
///
/// **`git diff` cannot see these and never will** — see [`Status::untracked`]
/// in `core` for why the status pass is what sources them.
///
/// The read goes through the entry's raw bytes, not their lossy spelling:
/// a name git reported is a name on disk, and joining through a decode would
/// look for a file that is not there. An unreadable file skips rather than
/// fails — a broken symlink, a socket and a file deleted between the two git
/// calls are all untracked, and none of them is worth refusing to show the
/// rest of the diff over.
fn loose_pair(entry: &UntrackedEntry, root: &Path) -> Option<Pair> {
    let content = std::fs::read(join_raw(root, entry.path.as_bytes())).ok()?;
    let binary = is_binary(&content);
    Some(Pair {
        // The one decode on this path: the pair's paths are display forms, and
        // everything that addressed the filesystem above was bytes.
        path: entry.path.to_string_lossy().into_owned(),
        old_path: None,
        // The same letter `git diff --raw` uses for a file that was added, so
        // nothing downstream has to learn that untracked is a category.
        status: 'A',
        old: Vec::new(),
        new: if binary { Vec::new() } else { lines(&content) },
        // Nothing on either side is in the object database: an untracked file
        // is on disk and nowhere else, so there is no OID pair to remember it
        // by — every diff of it is computed, never cached.
        old_oid: None,
        new_oid: None,
        binary,
    })
}

// ----------------------------------------------------------------------- refs

/// The `for-each-ref` format for local branches: full name, commit, the
/// remote-tracking ref the upstream resolves to, the two configured halves
/// of the upstream, its track state, and HEAD's marker.
///
/// Three atoms describe one upstream because they answer different
/// questions: `%(upstream)` names the local ref a fetch updates — the only
/// safe thing to *count* against, since `%(upstream:remoteref)` resolves in
/// the remote's own namespace and means nothing here — while the remotename
/// and remoteref pair is what the configuration literally says, which is
/// what the model carries.
const BRANCH_FORMAT: &str = concat!(
    "%(refname)%00%(objectname)%00%(upstream)",
    "%00%(upstream:remotename)%00%(upstream:remoteref)",
    "%00%(upstream:track)%00%(HEAD)"
);

/// Fields per [`BRANCH_FORMAT`] record.
const BRANCH_ARITY: usize = 7;

/// A full local ref starts here; whatever follows is the branch's own name.
const HEADS_PREFIX: &[u8] = b"refs/heads/";

/// A full tag ref starts here, for the same reason.
const TAGS_PREFIX: &[u8] = b"refs/tags/";

/// The name under a namespace: everything after `refs/heads/`, or all of it
/// when the prefix is not there — an honest echo of what git said beats a
/// guess at what it meant.
fn short<'a>(refname: &'a [u8], namespace: &[u8]) -> &'a [u8] {
    refname.strip_prefix(namespace).unwrap_or(refname)
}

/// Output up to its one trailing newline.
fn trimmed(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

/// The display form of bytes git emitted.
///
/// Where this lands in a *model field*, the field is human text — a stash
/// message, a reflog subject — and the comment there says why decoding
/// becomes right at that point. Names never come through here; they stay
/// [`PathBytes`] end to end because verbs aim them back at git.
fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// One `for-each-ref` record over local branches, before counts run.
#[derive(Debug)]
struct RawBranch {
    /// The full ref, `refs/heads/main` — kept whole because this is what
    /// addresses git; the short name is cut only when the model is built.
    refname: PathBytes,
    commit: String,
    head: bool,
    upstream: Option<RawUpstream>,
}

/// The tracking pair as configuration holds it, plus whether comparing
/// against it is even possible.
#[derive(Debug)]
struct RawUpstream {
    remote: PathBytes,
    /// The merge ref on the remote side, e.g. `refs/heads/main`.
    upstream_ref: PathBytes,
    /// The local ref a fetch updates for this pair, e.g.
    /// `refs/remotes/origin/main` — what counts are measured against, and
    /// not derivable from the two halves above without guessing where the
    /// remote's name ends.
    tracking_ref: PathBytes,
    track: Track,
}

/// Whether a branch can be compared against its upstream at all.
///
/// `%(upstream:track)` answers two cases outright — "" (equal) and "[gone]"
/// (the upstream's ref no longer exists locally) — and both retire the
/// rev-list that measures everything else. Its remaining values carry the
/// counts in prose, which are never parsed: they are re-measured exactly by
/// `rev-list --left-right --count`, where the numbers are numbers.
enum Track {
    Synced,
    Gone,
    Diverged,
}

impl std::fmt::Debug for Track {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Track::Synced => "synced",
            Track::Gone => "gone",
            Track::Diverged => "diverged",
        })
    }
}

fn track(field: &[u8]) -> Track {
    match field {
        b"" => Track::Synced,
        b"[gone]" => Track::Gone,
        _ => Track::Diverged,
    }
}

/// `for-each-ref` over `refs/heads`, in the framing described on
/// [`BRANCH_FORMAT`]: newline-terminated records of NUL-separated fields.
///
/// Empty fields are preserved, not filtered — "no upstream configured"
/// arrives as two empty pieces between NULs, and filtering empties would
/// slide every later field into their slots. A line that does not split into
/// exactly [`BRANCH_ARITY`] pieces is skipped whole: git prefixes warning
/// lines to this output, and one malformed record must not shift another's
/// fields.
fn parse_branches(raw: &[u8]) -> Vec<RawBranch> {
    let mut out = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&[u8]> = line.split(|b| *b == 0).collect();
        if f.len() != BRANCH_ARITY {
            continue;
        }
        let upstream = match (f[2].is_empty(), f[3].is_empty(), f[4].is_empty()) {
            // All three atoms or nothing: half a pair names no upstream, so
            // none is claimed rather than one guessed from partial words.
            (false, false, false) => Some(RawUpstream {
                tracking_ref: PathBytes::from_bytes(f[2]),
                remote: PathBytes::from_bytes(f[3]),
                upstream_ref: PathBytes::from_bytes(f[4]),
                track: track(f[5]),
            }),
            _ => None,
        };
        out.push(RawBranch {
            refname: PathBytes::from_bytes(f[0]),
            commit: lossy(f[1]),
            head: f[6].first() == Some(&b'*'),
            upstream,
        });
    }
    out
}

/// `for-each-ref` over `refs/remotes`: `refs/remotes/<remote>/<branch>` and
/// the commit, one per line.
///
/// The first slash after the namespace divides remote from branch — the same
/// convention every git reader applies, because a slash *inside* a remote
/// name is unreadable through any flat ref listing, this one included. The
/// symbolic `<remote>/HEAD` alias a clone writes beside the real branches is
/// not a branch and is dropped.
fn parse_remote_branches(raw: &[u8]) -> Vec<RemoteBranch> {
    let mut out = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&[u8]> = line.split(|b| *b == 0).collect();
        let [refname, oid] = f[..] else { continue };
        let rest = match refname.strip_prefix(b"refs/remotes/") {
            Some(rest) => rest,
            None => continue,
        };
        let mut halves = rest.splitn(2, |b| *b == b'/');
        let (remote, branch) = match (halves.next(), halves.next()) {
            (Some(r), Some(b)) if !b.is_empty() => (r, b),
            _ => continue,
        };
        if branch == b"HEAD" {
            continue;
        }
        out.push(RemoteBranch {
            remote: PathBytes::from_bytes(remote),
            branch: PathBytes::from_bytes(branch),
            commit: lossy(oid),
        });
    }
    out
}

/// The name stash `index` answers to: `stash@{n}`, bytes because it is an
/// address and not text.
///
/// Derived fresh at every verb, never stored: a drop renumbers everything
/// above it — the former `stash@{1}` *is* `stash@{0}` afterwards — and git's
/// numbering is the only truth there is. Re-deriving per call is what keeps a
/// held index from quietly aiming at whatever moved into its slot.
fn stash_ref(index: usize) -> Vec<u8> {
    format!("stash@{{{index}}}").into_bytes()
}

/// `git stash list -z --format=%H%x00%gs`, parsed.
///
/// `-z` ends every entry with NUL instead of a newline — the message inside
/// may hold any character a commit message may, newlines included — so the
/// stream splits into fixed pairs of fields with no line anywhere in it.
///
/// The index is the position: the list **is** the stash reflog read
/// newest-first, so entry `i` *is* `stash@{i}`, which is why `%gd` is not
/// asked for and re-derived from nothing.
///
/// The message decodes lossily, deliberately, exactly here: it is human text
/// shown to a person, and no verb ever aims at it — stashes are addressed by
/// [`Stash::index`] — so the raw bytes have nowhere further to travel and a
/// bad byte becomes U+FFFD instead of failing the whole stack.
fn parse_stashes(raw: &[u8]) -> Vec<Stash> {
    let fields: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    fields
        .chunks(2)
        .enumerate()
        .filter_map(|(index, rec)| match rec {
            [commit, message] => Some(Stash {
                index,
                commit: lossy(commit),
                message: String::from_utf8_lossy(message).into_owned(),
            }),
            _ => None,
        })
        .collect()
}

/// `git remote -v`: one line per URL as `<name>\t<url> (fetch)` or
/// `(push)`.
///
/// Neither half of a line can contain a newline — remote names are ref
/// components and config values cannot hold a raw one — so lines frame
/// records without a stronger separator. The same URL serving both
/// directions is listed once; an explicit distinct push URL is kept beside
/// the fetch URL, in the order git reported them.
fn parse_remotes(raw: &[u8]) -> Vec<Remote> {
    let mut out: Vec<Remote> = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Some(tab) = line.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let body = &line[tab + 1..];
        let url = match body
            .strip_suffix(b" (fetch)")
            .or_else(|| body.strip_suffix(b" (push)"))
        {
            Some(url) => url,
            None => continue,
        };
        let name = PathBytes::from_bytes(&line[..tab]);
        // URLs display and are never aimed at anything — verbs address the
        // remote by name — so this decode loses nothing that gets used.
        let url = String::from_utf8_lossy(url).into_owned();
        match out.iter_mut().find(|r| r.name == name) {
            Some(remote) => {
                if !remote.urls.contains(&url) {
                    remote.urls.push(url);
                }
            }
            None => out.push(Remote {
                name,
                urls: vec![url],
            }),
        }
    }
    out
}

/// `for-each-ref` over `refs/tags`: refname, peeled commit, object itself.
///
/// `%(*objectname)` peels an annotated tag to the commit it names and comes
/// back empty for a lightweight tag, whose own object already *is* the
/// commit — so the model carries the commit either way, positionally.
fn parse_tags(raw: &[u8]) -> Vec<Tag> {
    let mut out = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&[u8]> = line.split(|b| *b == 0).collect();
        let [refname, peeled, object] = f[..] else {
            continue;
        };
        out.push(Tag {
            name: PathBytes::from_bytes(short(refname, TAGS_PREFIX)),
            commit: lossy(if peeled.is_empty() { object } else { peeled }),
        });
    }
    out
}

/// `git reflog show --format=%h%x00%gd%x00%gs`, newest first.
///
/// Lines frame records because a reflog subject is single-line by
/// construction — git folds newlines away when it writes an entry — and NULs
/// split the three fields inside one.
///
/// Both text fields decode lossily, deliberately, at this boundary: the
/// subject is history shown to a person and the selector (`HEAD@{3}`) is
/// ASCII git itself generated. Neither addresses an object the way a branch
/// name does, so there is no verb these bytes have to survive.
fn parse_reflog(raw: &[u8]) -> Vec<ReflogEntry> {
    let mut out = Vec::new();
    for line in raw.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&[u8]> = line.split(|b| *b == 0).collect();
        let [commit, selector, message] = f[..] else {
            continue;
        };
        out.push(ReflogEntry {
            commit: lossy(commit),
            selector: String::from_utf8_lossy(selector).into_owned(),
            message: String::from_utf8_lossy(message).into_owned(),
        });
    }
    out
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
///
/// The paths are [`PathBytes`] — raw, undecoded — because the working-tree
/// read on the new side of a null-OID record addresses the filesystem with
/// them, and a decode there would look for a file git never named. The modes
/// and OIDs are ASCII by git's own grammar.
#[derive(Debug, PartialEq, Eq)]
struct RawChange {
    path: PathBytes,
    old_path: Option<PathBytes>,
    status: char,
    old_mode: String,
    new_mode: String,
    old_oid: String,
    new_oid: String,
}

impl RawChange {
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
/// Bytes end to end, with the same framing discipline as the porcelain v2
/// parser: every path slot is consumed exactly once, present or not, so one
/// malformed record can never shift the stream and rename somebody else's
/// file. A path is whatever bytes git emitted — no decode happens until a
/// [`Pair`] is built for display.
///
/// A record starting with *two* colons is a combined diff (`::100644 100644
/// 100644 … MM`): git only emits one for a merge when asked, and it carries N
/// modes, N OIDs and an N-letter status. Decoding that into this parser's five
/// positional slots fabricates data — mode in place of OID, a hex digit in place
/// of a status — so such a record is refused, not guessed at. The show path
/// passes `--diff-merges=first-parent`, which keeps git from sending any.
fn parse_raw(raw: &[u8]) -> Vec<RawChange> {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let mut out = Vec::new();
    let mut fields = raw.split(|b| *b == 0);
    while let Some(meta) = fields.next() {
        // The record proper starts after the last line break in this piece:
        // `git show` prefixes a newline (or more) before the first record.
        let Some(meta) = meta
            .rsplit(|b| *b == b'\n')
            .next()
            .and_then(|m| m.strip_prefix(b":"))
        else {
            continue;
        };
        // A second leading colon marks a combined record: N modes, N oids and an
        // N-letter status that this fixed-slot parser would read as garbage. With
        // --diff-merges=first-parent git cannot send one; refuse rather than decode.
        if meta.first() == Some(&b':') {
            continue;
        }
        let parts: Vec<&[u8]> = meta
            .split(|b| *b == b' ')
            .filter(|p| !p.is_empty())
            .collect();
        // mode_old mode_new oid_old oid_new status
        if parts.len() < 5 {
            continue;
        }
        let status = parts[4].first().copied().unwrap_or(b'M') as char;
        // One path field per record, consumed whether it parses or not; an
        // empty field is an absent path, and the record behind it is skipped —
        // but never left for the next record to trip over.
        let Some(first) = fields.next().filter(|p| !p.is_empty()) else {
            continue;
        };
        // R and C are the only statuses with a second path, and it is the one
        // the file is called now.
        let (old_path, path) = match status {
            'R' | 'C' => match fields.next().filter(|p| !p.is_empty()) {
                Some(second) => (
                    Some(PathBytes::from_bytes(first)),
                    PathBytes::from_bytes(second),
                ),
                None => (None, PathBytes::from_bytes(first)),
            },
            _ => (None, PathBytes::from_bytes(first)),
        };
        out.push(RawChange {
            path,
            old_path,
            status,
            old_mode: text(parts[0].strip_prefix(b":").unwrap_or(parts[0])),
            new_mode: text(parts[1]),
            old_oid: text(parts[2]),
            new_oid: text(parts[3]),
        });
    }
    out
}

/// Whether one side of a [`RawChange`] has a blob on the wire: not a gitlink,
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
fn new_side(oid: &str, repo: &Path, path: &[u8]) -> Option<Vec<u8>> {
    // Reached only when the fetch had nothing for the new side. The ordinary
    // case is a null OID: the content is the working tree's and nowhere else.
    // The extraordinary case is newer git hashing worktree content for a
    // rename record (an R068 carrying a real OID for a blob that was never
    // written to the object database) — the fetch answers "missing", and the
    // worktree is still the remaining truth. The old side has no equivalent
    // of either: a null OID there means the file did not exist, and reading
    // the working tree for it would diff an added file against itself and
    // report that nothing changed.
    let _ = oid;
    std::fs::read(join_raw(repo, path)).ok()
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
    use gitten_core::refs::RefName;

    /// A throwaway repository, because untracked files and conflicts are the
    /// things that cannot be tested against a canned string: they exist only
    /// relative to a real working tree.
    ///
    /// Every repository is initialized on `main` explicitly, so no test can
    /// inherit whatever a machine's `init.defaultBranch` says; every command
    /// pins identity and signing off, so nothing leaks in from a global or
    /// system config either.
    struct Scratch(std::path::PathBuf);

    /// The flags every scratch command runs under, so a test sees only what it
    /// set up itself: identity (no ambient user), signing off (a machine with
    /// `commit.gpgsign` must not fail these), and the local-file protocol
    /// (submodules over a path, which modern git disables by default).
    const SCRATCH_CONFIG: [&str; 10] = [
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "status.renames=true",
        // A machine without a global default names unborn branches `master`,
        // and a bare repository's HEAD then dangles on it: clones are born on
        // a branch no push ever named, and `push origin main` cannot resolve.
        // The tests' branch names are spelled; so is the default.
        "-c",
        "init.defaultBranch=main",
    ];

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("gitten-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let me = Scratch(dir);
            me.git(&["init", "-q", "-b", "main", "."]);
            // Repo-local identity: a commit needs an author, and a machine
            // without a global one — a fresh CI runner, a container — refuses
            // every commit with "Author identity unknown". The tests must not
            // care whose machine they run on.
            me.git(&["config", "user.name", "gitten-test"]);
            me.git(&["config", "user.email", "test@gitten.local"]);
            me
        }

        /// A scratch repository acting as a remote: bare, because nothing ever
        /// checks it out.
        fn bare(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("gitten-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let me = Scratch(dir);
            me.git(&["init", "-q", "--bare", "."]);
            me
        }

        /// A clone of an existing repository at its own path, standing in for
        /// the second machine a real divergence between two branches needs.
        fn cloned(from: &std::path::Path, name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("gitten-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let mut cmd = Command::new("git");
            cmd.arg("-C").arg(std::env::temp_dir());
            cmd.args(SCRATCH_CONFIG);
            cmd.args(["clone", "-q"]).arg(from).arg(&dir);
            let out = cmd.output().expect("git clone runs");
            assert!(
                out.status.success(),
                "clone: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            Scratch(dir)
        }

        fn git(&self, args: &[&str]) {
            self.git_os(
                &args
                    .iter()
                    .map(|a| std::ffi::OsStr::new(*a).to_owned())
                    .collect::<Vec<_>>(),
            );
        }

        /// Every scratch command runs under [`SCRATCH_CONFIG`] plus the local-
        /// file protocol allowance; this is where the two are attached.
        fn cmd(&self, args: &[std::ffi::OsString]) -> Command {
            let mut cmd = Command::new("git");
            cmd.arg("-C").arg(&self.0);
            cmd.args(SCRATCH_CONFIG);
            cmd.args(["-c", "protocol.file.allow=always"]);
            cmd.args(args);
            cmd
        }

        /// The same, for arguments that are paths rather than text: git takes
        /// filenames as bytes on Unix, and a non-UTF-8 name is exactly what
        /// some of these tests are about.
        fn git_os(&self, args: &[std::ffi::OsString]) {
            let out = self.cmd(args).output().expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        /// For the calls whose *failure* is the point — a conflicted merge
        /// exits nonzero and leaves the state the test wants.
        fn git_failing(&self, args: &[&str]) {
            let _ = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(SCRATCH_CONFIG)
                .args(args)
                .output()
                .expect("git runs");
        }

        /// The full object id of whatever `rev` resolves to, so assertions
        /// compare against git's own answer rather than a canned constant.
        fn rev_parse(&self, rev: &str) -> String {
            String::from_utf8(self.git_os_out(&["rev-parse".into(), rev.into()]))
                .unwrap()
                .trim()
                .to_string()
        }

        fn write(&self, path: &str, content: &[u8]) {
            self.write_bytes(path.as_bytes(), content);
        }

        /// A file whose *name* is bytes, not text — the shape a non-UTF-8
        /// pathname actually has on disk.
        ///
        /// False where the volume refuses the name outright. A filesystem that
        /// validates UTF-8 — macOS's APFS — answers `EILSEQ` for any name that
        /// is not text, which is a property of the volume and not of this
        /// code; byte-preserving Unix filesystems take the bytes as they come.
        /// Where it refuses there is nothing on disk to read, and the test
        /// falls back to what the machine makes possible.
        fn plant_raw(&self, path: &[u8], content: &[u8]) -> bool {
            let at = join_raw(&self.0, path);
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent).expect("a parent");
            }
            match std::fs::write(at, content) {
                Ok(()) => true,
                Err(e) if refused_name(&e) => false,
                Err(e) => panic!("a file: {e}"),
            }
        }

        fn write_bytes(&self, path: &[u8], content: &[u8]) {
            assert!(self.plant_raw(path, content), "the name was refused");
        }

        /// git with byte arguments, tolerating failure: true when it ran
        /// clean. A `git mv` *onto* a non-UTF-8 name fails the same way
        /// creating one does on a UTF-8-validating volume, and the caller has
        /// the same fallback.
        fn git_os_try(&self, args: &[std::ffi::OsString]) -> bool {
            self.cmd(args).output().expect("git runs").status.success()
        }

        /// git with byte arguments, output captured: for the plumbing calls
        /// that answer a question (`hash-object`).
        fn git_os_out(&self, args: &[std::ffi::OsString]) -> Vec<u8> {
            let out = self.cmd(args).output().expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        }

        fn open(&self) -> Handle {
            open(&self.0)
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

    /// `EILSEQ`, the errno a UTF-8-validating filesystem answers when asked
    /// for a pathname that is not valid UTF-8 — 92 on the BSD lineage (macOS),
    /// 84 on Linux. Two integers nobody renumbers, and not worth a libc
    /// dependency to spell.
    const EILSEQ_BSD: i32 = 92;
    const EILSEQ_LINUX: i32 = 84;

    /// Whether the volume itself refused a pathname. APFS validates UTF-8 and
    /// there is nowhere for such bytes to go; a byte-preserving filesystem
    /// never answers this.
    fn refused_name(e: &std::io::Error) -> bool {
        matches!(e.raw_os_error(), Some(EILSEQ_BSD | EILSEQ_LINUX))
    }

    /// The lines as plain slices, for assertions.
    fn strs(lines: &[Arc<str>]) -> Vec<&str> {
        lines.iter().map(|l| l.as_ref()).collect()
    }

    /// A repository with a real remote — a bare scratch repository reached
    /// over its local path, no network — with one commit pushed under `-u`,
    /// so `main` tracks `origin/main` and the remote-tracking ref exists.
    /// That is the least state ahead/behind and gone-ness mean anything in.
    fn upstream_fixture(name: &str) -> (Scratch, Scratch) {
        let origin = Scratch::bare(&format!("{name}-origin"));
        let r = Scratch::new(name);
        r.write("seed.txt", b"seed\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "seed"]);
        r.git(&[
            "remote",
            "add",
            "origin",
            &format!("{}", origin.0.display()),
        ]);
        r.git(&["push", "-q", "-u", "origin", "main"]);
        (r, origin)
    }

    fn branch<'a>(branches: &'a [Branch], name: &str) -> &'a Branch {
        branches
            .iter()
            .find(|b| b.name.as_bytes() == name.as_bytes())
            .unwrap_or_else(|| panic!("no branch {name} among {branches:?}"))
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

        let got = r.open().pairs("").expect("a working tree diff");
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
        let got = open(&r.0.join("sub"))
            .pairs("")
            .expect("a working tree diff");

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

        assert_eq!(paths(&r.open().pairs("").unwrap()), vec!["kept.txt"]);
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

        assert_eq!(paths(&r.open().pairs("").unwrap()), vec!["a/b/c.txt"]);
    }

    #[test]
    fn a_path_with_a_space_survives_because_the_records_are_nul_separated() {
        // `git status` *quotes* such a path without `-z`. With it the pair is
        // named exactly, and the file read succeeds.
        let r = Scratch::new("spaced");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("has space.txt", b"spaced\n");

        let got = r.open().pairs("").unwrap();
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
        let _ = open(&r.0).pairs(&format!("{hostile}..HEAD"));
        assert!(
            !target.exists(),
            "a revspec must not be able to make git write a file"
        );

        // The bare-revision (show) arm.
        let _ = open(&r.0).pairs(&hostile);
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

        let got = r.open().pairs("").unwrap();
        assert_eq!(paths(&got), vec!["blob.png"]);
        assert!(got[0].binary);
        assert!(got[0].new.is_empty(), "a binary carries no lines");
    }

    #[test]
    fn a_non_utf8_untracked_file_is_found_and_read_through_its_real_name() {
        // Latin-1 é in an on-disk name. If the working-tree read joined
        // through the lossy spelling it would stat `caf\u{FFFD}.txt` — a file
        // nobody ever created — miss, and drop the file from the diff
        // entirely. Its presence below IS the proof of byte-exact addressing,
        // and its contents are the proof of a successful read.
        let r = Scratch::new("raw-untracked");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        if !r.plant_raw(b"nouveau caf\xe9.txt", b"from disk\n") {
            // This volume validates UTF-8 (APFS) and refused the name; the
            // assertions below need the file to exist somewhere. The pipeline
            // itself is still proven byte-exact by the plumbed test and the
            // parser tests.
            return;
        }

        let got = r.open().pairs("").unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].status, 'A');
        assert_eq!(
            got[0].path, "nouveau caf\u{FFFD}.txt",
            "display decodes lossily"
        );
        assert_eq!(
            strs(&got[0].new),
            ["from disk"],
            "read through the name git reported, not a decoded near-miss"
        );
    }

    #[test]
    fn a_non_utf8_tracked_modification_reads_both_sides() {
        let r = Scratch::new("raw-tracked");
        if !r.plant_raw(b"caf\xe9.txt", b"before\n") {
            // Refused by this volume; see the untracked test. The tracked
            // path through the object database is proven everywhere by
            // `a_non_utf8_path_in_history_arrives_whole`.
            return;
        }
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.plant_raw(b"caf\xe9.txt", b"after\n");

        let got = r.open().pairs("").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].status, 'M');
        assert_eq!(got[0].path, "caf\u{FFFD}.txt");
        assert_eq!(strs(&got[0].old), ["before"], "blob fetch keyed on the OID");
        assert_eq!(
            strs(&got[0].new),
            ["after"],
            "the null-OID side was read off disk by its real bytes"
        );
    }

    #[test]
    fn a_non_utf8_path_in_history_arrives_whole_from_the_object_database() {
        // The one shape every filesystem can produce: git's index and trees
        // carry pathnames as bytes regardless of what the working volume
        // accepts, so plumbing can record a path here that no `creat` on this
        // volume would. From there it is the whole acquisition pipeline under
        // test — raw parse, batch alignment, pair labelling — with no
        // filesystem read in the way.
        use std::os::unix::ffi::OsStrExt;
        let r = Scratch::new("raw-plumbed");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);

        // Hash the blob from an ordinary file, then attach that OID to the
        // raw-byte pathname in the index; the value argument is built as
        // bytes so the path rides inside it verbatim.
        r.write("content.tmp", b"planted\n");
        let oid = String::from_utf8(r.git_os_out(&[
            "hash-object".into(),
            "-w".into(),
            "content.tmp".into(),
        ]))
        .unwrap();
        let mut value: Vec<u8> = format!("100644,{},", oid.trim()).into_bytes();
        value.extend_from_slice(b"caf\xe9.txt");
        let add = [
            std::ffi::OsStr::new("update-index").to_owned(),
            std::ffi::OsStr::new("--add").to_owned(),
            std::ffi::OsStr::new("--cacheinfo").to_owned(),
            std::ffi::OsStr::from_bytes(&value).to_owned(),
        ];
        r.git_os(&add);
        r.git(&["commit", "-qm", "planted"]);

        let got = r.open().pairs("HEAD~1..HEAD").unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].status, 'A');
        assert_eq!(
            got[0].path, "caf\u{FFFD}.txt",
            "display decodes lossily, once, at the pair"
        );
        assert_eq!(
            strs(&got[0].new),
            ["planted"],
            "the blob landed on the file whose bytes named it"
        );
    }

    /// The cache's end-to-end contract, over real git output: an unchanged
    /// blob pair is diffed once and remembered; a side with no OID — the
    /// untracked file here — computes every time, because it has no identity
    /// to be remembered under.
    #[test]
    fn a_second_diff_of_the_same_tree_is_remembered_not_recomputed() {
        use gitten_core::differ::{Differ, Edit};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Counting(Arc<AtomicUsize>);
        impl Differ for Counting {
            fn name(&self) -> &'static str {
                "counting"
            }
            fn diff(&self, _p: &str, old: &[Arc<str>], new: &[Arc<str>]) -> Vec<Edit> {
                self.0.fetch_add(1, Ordering::Relaxed);
                vec![Edit {
                    old_start: 0,
                    old_end: old.len() as u32,
                    new_start: 0,
                    new_end: new.len() as u32,
                }]
            }
        }

        let r = Scratch::new("cache-e2e");
        r.write("f.txt", b"one\ntwo\nthree\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "one"]);
        r.write("f.txt", b"one\nTWO\nthree\nfour\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "two"]);
        // One more edit, staged but not committed. A clean worktree would leave
        // the `""` pass holding only the untracked file; a staged one puts in it
        // a pair whose *both* sides are blobs — old from commit two, new from
        // the index — which is what gives the worktree pass something to hit.
        r.write("f.txt", b"one\nTWO\nthree\nfour\nfive\n");
        r.git(&["add", "-A"]);
        // Untracked: in no commit and no index, so no OID on either side.
        r.write("loose.txt", b"untracked\n");

        let calls = Arc::new(AtomicUsize::new(0));
        let mut differs = Differs::builtin();
        differs.register(Counting(Arc::clone(&calls)));
        assert!(differs.select("counting"));

        let g = r.open();
        let first = diff(g.as_ref(), "", &differs, &Overrides::default()).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2, "cold pass: both files");

        let second = diff(g.as_ref(), "", &differs, &Overrides::default()).unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            3,
            "the committed pair hit (no new call); untracked computed again"
        );
        assert_eq!(first, second, "a hit is byte-identical to what a miss said");

        // And a range with nothing loose in it settles completely: the second
        // pass adds no computation at all.
        let a = diff(g.as_ref(), "HEAD~1..HEAD", &differs, &Overrides::default()).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        let b = diff(g.as_ref(), "HEAD~1..HEAD", &differs, &Overrides::default()).unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            4,
            "same tree twice is one computation"
        );
        assert_eq!(a, b);
    }

    #[test]
    fn a_rename_to_a_non_utf8_name_keeps_identity_through_the_read() {
        // Renamed in the index, then edited outside it: the raw record is an
        // R carrying both names, the new side's OID is null, and reading it
        // means addressing the filesystem with the new name's exact bytes.
        use std::os::unix::ffi::OsStrExt;
        let r = Scratch::new("raw-rename");
        r.write("before.txt", b"moved contents\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);

        let to: std::ffi::OsString = std::ffi::OsStr::from_bytes(b"aft\xe9r.txt").to_owned();
        let moved = r.git_os_try(&["mv".into(), "before.txt".into(), to.clone()]);
        if !moved {
            // This volume validates UTF-8 and refused the destination; both
            // names byte-exact is still proven at the parser, and the whole
            // pipeline by the plumbed test.
            return;
        }
        r.plant_raw(b"aft\xe9r.txt", b"moved contents\nedited\n");

        let g = r.open();
        let got = g.pairs("").unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].status, 'R');
        assert_eq!(got[0].old_path.as_deref(), Some("before.txt"));
        assert_eq!(got[0].path, "aft\u{FFFD}r.txt", "display decodes lossily");
        assert_eq!(
            strs(&got[0].new),
            ["moved contents", "edited"],
            "read through the renamed file's real bytes"
        );

        // And the model keeps both names byte-exact for whoever addresses or
        // stages them later.
        let s = g.status().unwrap();
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].path.as_bytes(), b"aft\xe9r.txt".as_slice());
        assert_eq!(
            s.staged[0].old_path.as_ref().map(PathBytes::as_bytes),
            Some(b"before.txt".as_slice())
        );
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

        let g = r.open();
        assert_eq!(paths(&g.pairs("HEAD~1..HEAD").unwrap()), vec!["a.txt"]);
        assert!(
            paths(&g.pairs("").unwrap()).contains(&"loose.txt"),
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

        let got = r.open().pairs("HEAD~1..HEAD").unwrap();
        assert_eq!(paths(&got), vec!["a.txt", "b.txt", "c.txt", "d.txt"]);
        let new: Vec<_> = got.iter().map(|p| p.new.join("\n")).collect();
        assert_eq!(new, vec!["A", "B", "same bytes", "same bytes"]);
    }

    // ------------------------------------------------------------------ status

    #[test]
    fn a_clean_tree_is_an_empty_status() {
        let r = Scratch::new("clean");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);

        let s = r.open().status().unwrap();
        assert!(s.is_empty(), "{s:?}");
        // And every list individually, because `is_empty` could be lying.
        assert!(s.staged.is_empty() && s.unstaged.is_empty());
        assert!(s.untracked.is_empty() && s.conflicts.is_empty() && s.ignored.is_empty());
    }

    #[test]
    fn staged_and_unstaged_are_separate_answers_about_one_file() {
        // Edited, staged, edited again: porcelain v1 had two letters for this;
        // the model has two lists, and this is why they are lists and not one.
        let r = Scratch::new("both-sides");
        r.write("f.txt", b"original\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("f.txt", b"staged version\n");
        r.git(&["add", "f.txt"]);
        r.write("f.txt", b"staged version\nand more\n");

        let s = r.open().status().unwrap();
        assert_eq!(
            s.staged
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>(),
            vec!["f.txt"],
            "the index has its own opinion"
        );
        assert_eq!(s.staged[0].change, Change::Modified);
        assert_eq!(
            s.unstaged
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>(),
            vec!["f.txt"],
            "so does the working tree"
        );
        assert!(s.untracked.is_empty());
    }

    #[test]
    fn a_staged_rename_reports_its_old_name() {
        let r = Scratch::new("mv");
        r.write("before.txt", b"contents\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.git(&["mv", "before.txt", "after.txt"]);
        // The application asks for renames explicitly. A user's contrary
        // status preference must not split this into an unrelated add/delete.
        r.git(&["config", "status.renames", "false"]);

        let s = r.open().status().unwrap();
        assert_eq!(s.staged.len(), 1);
        let e = &s.staged[0];
        assert_eq!(e.change, Change::Renamed);
        assert_eq!(e.path.as_bytes(), b"after.txt");
        assert_eq!(
            e.old_path.as_ref().map(|p| p.as_bytes()),
            Some(b"before.txt".as_slice()),
            "the name it had travels with it"
        );
    }

    #[test]
    fn a_merge_conflict_lists_itself_as_conflicted_and_nowhere_else() {
        let r = Scratch::new("conflict");
        r.write("shared.txt", b"base\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "base"]);
        r.git(&["checkout", "-qb", "other"]);
        r.write("shared.txt", b"theirs\n");
        r.git(&["commit", "-qam", "theirs"]);
        r.git(&["checkout", "-q", "main"]);
        r.write("shared.txt", b"ours\n");
        r.git(&["commit", "-qam", "ours"]);
        r.git_failing(&["merge", "other"]);

        let s = r.open().status().unwrap();
        assert_eq!(s.conflicts.len(), 1);
        let c = &s.conflicts[0];
        assert_eq!(c.path.as_bytes(), b"shared.txt");
        assert_eq!(c.state, ConflictKind::BothModified);
        assert_eq!(c.kind, Kind::File);
        assert!(
            s.staged.is_empty() && s.unstaged.is_empty(),
            "a conflicted path is not also staged or unstaged: its truth lives \
             in three index stages no single letter could name"
        );
    }

    #[test]
    fn untracked_and_ignored_land_in_their_own_lists() {
        let r = Scratch::new("sorting");
        r.write(".gitignore", b"junk/\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("loose.txt", b"x\n");
        r.write("junk/cache.tmp", b"y\n");

        let s = r.open().status().unwrap();
        assert_eq!(
            s.untracked
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>(),
            vec!["loose.txt"]
        );
        assert!(
            s.ignored.is_empty(),
            "gitten does not ask git for ignored files — target/ would be forty \
             thousand entries nobody reads"
        );
    }

    #[test]
    fn a_submodule_reads_as_one_with_its_flags() {
        // An upstream with two commits, borrowed at the older one and then
        // moved forward with a dirty file inside: the parent should see a
        // submodule whose commit changed and whose content was edited.
        let up = Scratch::new("sub-upstream");
        up.write("f.txt", b"one\n");
        up.git(&["add", "-A"]);
        up.git(&["commit", "-qm", "one"]);
        up.write("f.txt", b"one\ntwo\n");
        up.git(&["commit", "-qam", "two"]);
        let older = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&up.0)
                .args(["rev-parse", "HEAD~1"])
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .unwrap();

        let r = Scratch::new("sub-parent");
        r.write("base.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "base"]);
        r.git(&[
            "submodule",
            "add",
            "-q",
            &format!("{}", up.0.display()),
            "kid",
        ]);
        r.git(&["commit", "-qm", "borrow"]);
        // Move the borrow forward and dirty what is inside it.
        r.git(&["-C", "kid", "checkout", "-q", older.trim()]);
        r.write("kid/f.txt", b"one\ntwo\nthree\n");

        let s = r.open().status().unwrap();
        assert_eq!(s.unstaged.len(), 1, "{s:?}");
        let e = &s.unstaged[0];
        assert_eq!(e.path.as_bytes(), b"kid");
        assert_eq!(
            e.kind,
            Kind::Submodule,
            "mode 160000, not something to read"
        );
        assert!(e.submodule.commit_changed, "the borrowed commit moved");
        assert!(e.submodule.modified, "a file inside it was edited");
        assert!(!e.submodule.untracked);
    }

    #[test]
    fn a_staged_gitlink_change_does_not_inherit_the_worktrees_flags() {
        // The parent borrows the upstream's *newer* commit, moves the borrow
        // back and stages that, then dirties a file inside. One record —
        // `1 MM S.M.` — and the flags belong to the unstaged side alone:
        // C/M/U compare the submodule against the index or itself, which is
        // worktree business, so the staged entry (index against HEAD) carries
        // none of them.
        let up = Scratch::new("sub-up2");
        up.write("f.txt", b"one\n");
        up.git(&["add", "-A"]);
        up.git(&["commit", "-qm", "one"]);
        up.write("f.txt", b"one\ntwo\n");
        up.git(&["commit", "-qam", "two"]);

        let r = Scratch::new("sub-parent2");
        r.write("base.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "base"]);
        r.git(&[
            "submodule",
            "add",
            "-q",
            &format!("{}", up.0.display()),
            "kid",
        ]);
        r.git(&["commit", "-qm", "borrow"]);
        // The borrow moves back and that move is staged; then a file inside
        // is edited, which only the worktree knows about.
        r.git(&["-C", "kid", "checkout", "-q", "HEAD~1"]);
        r.git(&["add", "kid"]);
        r.write("kid/f.txt", b"one\ntwo\nthree\n");

        let s = r.open().status().unwrap();
        assert_eq!(s.staged.len(), 1, "{s:?}");
        let staged = &s.staged[0];
        assert_eq!(staged.change, Change::Modified);
        assert_eq!(staged.kind, Kind::Submodule);
        assert_eq!(
            staged.submodule,
            Submodule::default(),
            "nothing in S<C><M><U> says anything about index vs HEAD"
        );

        assert_eq!(s.unstaged.len(), 1);
        let unstaged = &s.unstaged[0];
        assert!(
            unstaged.submodule.modified,
            "the dirty file inside shows on the side that describes it"
        );
        assert!(
            !unstaged.submodule.commit_changed,
            "checkout now matches what the index records"
        );
    }

    #[test]
    fn an_ordinary_record_lands_on_the_side_that_changed() {
        let mut s = Status::default();
        ordinary(b"1 M. N... 100644 100644 100644 aa bb tracked.txt", &mut s);
        assert!(s.unstaged.is_empty(), "the dot side says nothing changed");
        assert_eq!(s.staged[0].change, Change::Modified);

        s = Status::default();
        ordinary(b"1 .D N... 100644 100644 000000 aa cc deleted.txt", &mut s);
        assert!(s.staged.is_empty());
        assert_eq!(s.unstaged[0].change, Change::Deleted);

        s = Status::default();
        ordinary(b"1 AM N... 100644 100644 100644 00 bb new.txt", &mut s);
        assert_eq!(s.staged[0].change, Change::Added);
        assert_eq!(s.unstaged[0].change, Change::Modified);
    }

    #[test]
    fn kinds_come_from_the_mode_of_their_own_column() {
        // A path the index records as a symlink and the working tree replaced
        // with a plain file: each entry reads the mode of the side it
        // describes, never the other one's.
        let mut s = Status::default();
        ordinary(b"1 MM N... 120000 120000 100644 aa bb link", &mut s);
        assert_eq!(
            s.staged[0].kind,
            Kind::Symlink,
            "the index still records it"
        );
        assert_eq!(s.unstaged[0].kind, Kind::File, "the worktree replaced it");
    }

    #[test]
    fn a_deletion_keeps_the_kind_of_the_side_that_existed() {
        // A deletion's destination column prints 000000, which parses as a
        // plain file — and would quietly relabel every deleted symlink and
        // every deleted submodule as text to read. The side that *did* exist
        // is in the same record.
        let mut s = Status::default();
        ordinary(b"1 .D N... 120000 120000 000000 aa bb link", &mut s);
        assert_eq!(s.unstaged[0].change, Change::Deleted);
        assert_eq!(
            s.unstaged[0].kind,
            Kind::Symlink,
            "deleted from the worktree; the index still said what it was"
        );

        s = Status::default();
        ordinary(b"1 D. N... 100644 000000 000000 aa 00 gone.txt", &mut s);
        assert_eq!(s.staged[0].change, Change::Deleted);
        assert_eq!(s.staged[0].kind, Kind::File);

        s = Status::default();
        ordinary(b"1 D. S... 160000 000000 000000 aa 00 kid", &mut s);
        assert_eq!(s.staged[0].change, Change::Deleted);
        assert_eq!(
            s.staged[0].kind,
            Kind::Submodule,
            "a deleted gitlink is not a file to read"
        );
    }

    #[test]
    fn a_conflict_without_a_worktree_side_falls_back_to_the_stages() {
        // A file removed mid-merge (or both sides having deleted it) prints
        // mW as 000000 while the stages still say what existed. Base, ours,
        // theirs, in that order; nothing anywhere guesses low.
        let mut s = Status::default();
        unmerged(
            b"u UU N... 120000 100644 100644 000000 h1 h2 h3 rm-during-merge",
            &mut s,
        );
        assert_eq!(
            s.conflicts[0].kind,
            Kind::Symlink,
            "the base stage says what the path was"
        );

        s = Status::default();
        unmerged(
            b"u DD N... 000000 000000 000000 000000 00 00 00 both-deleted.txt",
            &mut s,
        );
        assert_eq!(
            s.conflicts[0].kind,
            Kind::File,
            "nothing anywhere existed; the conservative guess stands"
        );
    }

    #[test]
    fn a_rename_carries_its_old_name_across_the_field_boundary() {
        // Straight off a real run, NULs made visible: the old name arrives as
        // the NEXT nul-delimited field, after the path.
        let raw = b"2 R. N... 100644 100644 100644 aa bb R100 renamed file.txt\0spaced name.txt";
        let s = parse_status(raw);
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].change, Change::Renamed);
        assert_eq!(s.staged[0].path.as_bytes(), b"renamed file.txt".as_slice());
        assert_eq!(
            s.staged[0].old_path.as_ref().map(|p| p.as_bytes()),
            Some(b"spaced name.txt".as_slice())
        );
    }

    #[test]
    fn a_copy_is_not_quietly_called_a_rename() {
        let raw = b"2 C. N... 100644 100644 100644 aa bb C075 copy.txt\0origin.txt";
        let s = parse_status(raw);
        assert_eq!(s.staged[0].change, Change::Copied);
        assert_eq!(
            s.staged[0].old_path.as_ref().map(|p| p.as_bytes()),
            Some(b"origin.txt".as_slice())
        );
    }

    #[test]
    fn one_rename_can_also_be_modified_in_the_worktree() {
        // `RM`: renamed in the index and further edited outside it. One record,
        // two lists — the shape v1 could not say.
        let old: &[u8] = b"old name.txt";
        let mut raw = b"2 RM N... 100644 100644 100644 aa bb R100 new name.txt".to_vec();
        raw.push(0);
        raw.extend_from_slice(old);
        let s = parse_status(&raw);
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.unstaged.len(), 1);
        assert_eq!(s.unstaged[0].change, Change::Modified);
        assert_eq!(
            s.staged[0].old_path.as_ref().map(PathBytes::as_bytes),
            Some(old)
        );
    }

    #[test]
    fn an_unmerged_record_names_how_the_sides_disagree() {
        // Both forms observed in the wild: the classic both-modified, and a
        // both-added where neither side ever had the file before.
        let raw = b"\
            u UU N... 100644 100644 100644 100644 h1 h2 h3 shared.txt\0\
            u AA N... 000000 100644 100644 100644 h1 h2 h3 new.txt\0\
            u DU N... 100644 000000 100644 100644 h1 h2 h3 theirs-deleted.txt\0";
        let s = parse_status(raw);
        assert_eq!(
            s.conflicts.iter().map(|c| c.state).collect::<Vec<_>>(),
            vec![
                ConflictKind::BothModified,
                ConflictKind::BothAdded,
                ConflictKind::DeletedByUs,
            ]
        );
        assert!(s.staged.is_empty() && s.unstaged.is_empty());
        assert_eq!(s.conflicts[1].kind, Kind::File);
    }

    #[test]
    fn untracked_and_ignored_records_land_apart() {
        let raw = b"? loose.txt\0? spaced out.txt\0! junk/cache.tmp\0";
        let s = parse_status(raw);
        assert_eq!(
            s.untracked
                .iter()
                .map(|e| e.path.as_bytes())
                .collect::<Vec<_>>(),
            [b"loose.txt".as_slice(), b"spaced out.txt".as_slice()]
        );
        assert_eq!(s.ignored.len(), 1);
        assert_eq!(s.ignored[0].as_bytes(), b"junk/cache.tmp");
    }

    #[test]
    fn a_malformed_record_is_skipped_without_eating_the_next_one() {
        // Every way the stream can go wrong — an unknown tag, short records, a
        // bad XY pair, an unknown conflict code, an empty path — and one good
        // record behind all of them, which must still arrive.
        //
        // A rename with no second field is not in this list, deliberately: by
        // protocol the next field after any `2` record *is* its old name, so a
        // truncated read there is indistinguishable from a rename whose old
        // name simply arrived. See the alignment test below for how that is
        // survived.
        let raw = b"\
            # some header nobody asked for\0\
            1 XZ N... 100644 100644 100644 aa bb broken.txt\0\
            1 M.\0\
            u ZZ N... 100644 100644 100644 100644 h1 h2 h3 weird.txt\0\
            1 M. N...\0\
            1 M. N... 100644 100644 100644 aa bb good.txt\0";
        let s = parse_status(raw);
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].path.as_bytes(), b"good.txt");
        assert!(s.unstaged.is_empty() && s.conflicts.is_empty());
    }

    #[test]
    fn a_rename_missing_its_old_name_still_shows() {
        // The second field never arrived (a truncated read). Dropping the whole
        // change would hide work; showing it under the new name alone loses
        // nothing but the old label.
        let raw = b"2 R. N... 100644 100644 100644 aa bb R100 lonely.txt";
        let s = parse_status(raw);
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].path.as_bytes(), b"lonely.txt");
        assert_eq!(s.staged[0].old_path, None);
    }

    #[test]
    fn a_rename_consumes_its_old_name_even_when_the_record_is_broken() {
        // The alignment trap: the old name is taken before the record parses,
        // so a malformed rename cannot leave the cursor sitting on somebody
        // else's path and corrupt two entries instead of zero.
        let mut raw = b"2 XZ N... bad bad bad aa bb R99 x\0old-name".to_vec();
        raw.push(0);
        raw.extend_from_slice(b"1 M. N... 100644 100644 100644 aa bb next.txt");
        raw.push(0);
        let s = parse_status(&raw);
        assert_eq!(
            s.staged
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>(),
            vec!["next.txt"],
            "exactly the records that make sense, nothing shifted by one"
        );
    }

    #[test]
    fn an_empty_origpath_field_is_consumed_exactly_and_not_swallowed() {
        // Regression: empty NUL fields used to be filtered out wholesale,
        // which made an *empty* origPath invisible — and the next valid
        // record slid into its slot and vanished with it. By position the
        // field after any `2` record is the old name; an empty one is a real
        // answer ("nothing arrived"), consumed like any other.
        let mut raw = b"2 XZ N... bad bad bad aa bb R99 x".to_vec();
        raw.push(0);
        // The empty field itself:
        raw.push(0);
        raw.extend_from_slice(b"1 M. N... 100644 100644 100644 aa bb next.txt");
        raw.push(0);
        let s = parse_status(&raw);
        assert_eq!(
            s.staged
                .iter()
                .map(|e| e.path.as_bytes())
                .collect::<Vec<_>>(),
            [b"next.txt".as_slice()],
            "the good record behind the empty field survived"
        );
        assert!(s.staged[0].old_path.is_none());
    }

    #[test]
    fn spaces_and_newlines_in_paths_arrive_whole() {
        // The reason for `-z`: a path may contain anything a filesystem allows,
        // including a newline, and only NUL ends it.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"? dir with spaces/a very long name.txt");
        raw.push(0);
        raw.extend_from_slice(b"? multi\nline.txt");
        raw.push(0);
        let s = parse_status(&raw);
        assert_eq!(
            s.untracked
                .iter()
                .map(|e| e.path.as_bytes())
                .collect::<Vec<_>>(),
            [
                b"dir with spaces/a very long name.txt".as_slice(),
                b"multi\nline.txt".as_slice(),
            ]
        );
    }

    #[test]
    fn a_non_utf8_path_arrives_byte_for_byte() {
        // Latin-1 é. Decoding lossily here would rename somebody's file; the
        // model keeps what git said and displays later, on its own terms.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"? caf\xe9.txt");
        raw.push(0);
        let s = parse_status(&raw);
        assert_eq!(s.untracked[0].path.as_bytes(), b"caf\xe9.txt".as_slice());
        // Display stays lossy rather than failing — never refuse a repository
        // over one byte.
        assert!(s.untracked[0].path.to_string().contains('\u{FFFD}'));
    }

    #[test]
    fn submodule_flags_come_from_the_state_field() {
        // S<C><M><U>, letters or dots, as git prints them; N means not a
        // submodule and claims no flags.
        let mut s = Status::default();
        ordinary(b"1 .M SCMU 160000 160000 160000 aa bb kid", &mut s);
        let sub = s.unstaged[0].submodule;
        assert!(sub.commit_changed && sub.modified && sub.untracked);

        s = Status::default();
        ordinary(b"1 A. S... 000000 160000 160000 00 bb kid", &mut s);
        assert_eq!(s.staged[0].kind, Kind::Submodule);
        assert_eq!(
            s.staged[0].submodule,
            Submodule::default(),
            "a clean borrow claims nothing"
        );

        s = Status::default();
        ordinary(b"1 .M N... 100644 100644 100644 aa bb plain.txt", &mut s);
        assert_eq!(s.unstaged[0].submodule, Submodule::default());

        // A state field shorter than the documented four characters claims no
        // flags either — a future git may change the spelling; kind survives.
        s = Status::default();
        ordinary(b"1 .M S 160000 160000 160000 aa bb odd", &mut s);
        assert_eq!(s.unstaged[0].kind, Kind::Submodule);
        assert_eq!(s.unstaged[0].submodule, Submodule::default());
    }

    #[test]
    fn an_empty_or_header_only_stream_is_an_empty_status() {
        assert_eq!(parse_status(b""), Status::default());
        assert_eq!(parse_status(b"\0\0"), Status::default());
        assert_eq!(
            parse_status(b"# branch.oid abc\0# branch.head main\0"),
            Status::default()
        );
    }

    // ------------------------------------------------------- raw-record parsing

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

        let got = open(&r.0).pairs(&merge).expect("a diff for a merge commit");
        assert!(!got.is_empty(), "a merge renders as a diff, not silence");

        // Git's own answer to the same question: ordinary records between
        // parent one and the merge, parsed with the same parser rather than
        // reimplementing its record format here.
        let expected = parse_raw(
            git(&[
                "diff",
                "--raw",
                "-z",
                "-M",
                "--no-ext-diff",
                &format!("{merge}^1"),
                &merge,
            ])
            .as_bytes(),
        );
        let want: std::collections::BTreeSet<String> = expected
            .iter()
            .map(|c| c.path.to_string_lossy().into_owned())
            .collect();
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
        let raw: &[u8] = b":100644 100644 aaa bbb M\0src/main.rs\0";
        assert_eq!(
            parse_raw(raw),
            vec![RawChange {
                path: PathBytes::from("src/main.rs"),
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
        let raw: &[u8] = b":100644 100644 aaa bbb R096\0old/name.rs\0new/name.rs\0";
        let c = parse_raw(raw);
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0].old_path.as_ref().map(PathBytes::as_bytes),
            Some(b"old/name.rs".as_slice())
        );
        assert_eq!(c[0].path.as_bytes(), b"new/name.rs");
        assert_eq!(c[0].status, 'R');
    }

    #[test]
    fn a_raw_path_with_a_non_utf8_byte_stays_byte_exact() {
        // Latin-1 é in the path field of a deletion. A lossy decode at this
        // boundary would rename the file before anyone even read it.
        let mut raw = b":100644 000000 aaa 00 D".to_vec();
        raw.push(0);
        raw.extend_from_slice(b"caf\xe9.txt");
        raw.push(0);
        let c = parse_raw(&raw);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].path.as_bytes(), b"caf\xe9.txt".as_slice());
        assert_eq!(c[0].status, 'D');
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
        let raw: &[u8] = b":100644 100644 a b M\0one.rs\0:000000 100644 0000000000000000000000000000000000000000 c A\0two.rs\0:100644 000000 d 0000000000000000000000000000000000000000 D\0three.rs\0";
        let c = parse_raw(raw);
        assert_eq!(c.len(), 3);
        assert_eq!(
            c.iter().map(|r| r.status).collect::<Vec<_>>(),
            vec!['M', 'A', 'D']
        );
        assert_eq!(c[2].path.as_bytes(), b"three.rs");
    }

    #[test]
    fn a_commit_header_before_the_records_is_skipped() {
        // `git show --format=` still emits a newline before the first record.
        let raw: &[u8] = b"\n:100644 100644 a b M\0x.rs\0";
        assert_eq!(parse_raw(raw).len(), 1);
    }

    #[test]
    fn nothing_changed_is_no_records_rather_than_an_error() {
        assert!(parse_raw(b"").is_empty());
        assert!(parse_raw(b"\0").is_empty());
        assert!(parse_raw(b"not a record\0").is_empty());
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
            parse_raw(raw.as_bytes()),
            vec![RawChange {
                path: PathBytes::from_bytes(b"keep.txt"),
                old_path: None,
                status: 'M',
                old_mode: "100644".into(),
                new_mode: "100644".into(),
                old_oid: "aaa".into(),
                new_oid: "bbb".into(),
            }],
            "the combined record never becomes a RawChange and no field of it leaks"
        );
    }

    #[test]
    fn a_raw_path_with_a_space_survives() {
        // The whole reason for `-z`. Splitting the metadata on whitespace is
        // safe only because the path is not in it.
        let raw: &[u8] = b":100644 100644 a b M\0dir with spaces/a file.rs\0";
        assert_eq!(
            parse_raw(raw)[0].path.as_bytes(),
            b"dir with spaces/a file.rs"
        );
    }

    #[test]
    fn a_submodule_bump_is_a_one_line_synthetic_file() {
        // A gitlink's OID is a commit in another repository. Fetching it as a
        // blob gets "missing", which reads on screen as a file that changed and
        // shows nothing — so it is synthesised the way git does.
        let raw: &[u8] = b":160000 160000 34cbf180d 5697db813 M\0ghostty\0";
        let c = &parse_raw(raw)[0];
        assert_eq!(c.old_mode, "160000");
        let old = RawChange::synthetic(&c.old_mode, &c.old_oid).expect("a gitlink is synthetic");
        let new = RawChange::synthetic(&c.new_mode, &c.new_oid).unwrap();
        assert_eq!(strs(&lines(&old)), ["Subproject commit 34cbf180d"]);
        assert_eq!(strs(&lines(&new)), ["Subproject commit 5697db813"]);

        // An added submodule has no old side at all.
        assert_eq!(
            RawChange::synthetic("160000", &"0".repeat(40)),
            Some(Vec::new())
        );
        // And an ordinary file is not synthetic, so it goes to `cat-file`.
        assert_eq!(RawChange::synthetic("100644", "aaa"), None);
    }

    #[test]
    fn a_gitlink_is_never_fetched_from_the_object_database() {
        // The exact predicate that builds the batch request and consumes its
        // answers; a gitlink or a null OID there would desynchronise the pair.
        assert!(fetchable("100644", "abc123"));
        assert!(!fetchable("160000", "34cbf180d"));
        assert!(!fetchable("100644", &"0".repeat(40)));
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
                    None,
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
            old_oid: None,
            new_oid: None,
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

    // -------------------------------------------------------------------- refs

    #[test]
    fn an_empty_repository_reads_as_unborn_and_every_list_reads_as_empty() {
        // Fresh `git init`: HEAD names a branch that has no commits yet, and
        // no branch ref, stash, tag, remote or reflog exists. Every read
        // answers — none of this is an error state, or opening a repository
        // the moment after creating it would fail.
        let r = Scratch::new("refs-empty");
        let g = r.open();

        assert_eq!(
            g.head().unwrap(),
            HeadState::Branch {
                name: RefName::from("main"),
                commit: None,
            },
            "an unborn branch is a name and nothing else yet"
        );
        assert_eq!(g.branches().unwrap(), vec![]);
        assert_eq!(g.remote_branches().unwrap(), vec![]);
        assert_eq!(g.stashes().unwrap(), vec![]);
        assert_eq!(g.remotes().unwrap(), vec![]);
        assert_eq!(g.tags().unwrap(), vec![]);
        assert_eq!(
            g.reflog(10).unwrap(),
            vec![],
            "the unborn branch has no reflog; that is emptiness, not breakage"
        );
    }

    #[test]
    fn local_branches_carry_their_commit_head_flag_and_upstream() {
        let (r, _) = upstream_fixture("ref-fields");
        r.git(&["branch", "feature"]);

        let got = r.open().branches().unwrap();
        let main = branch(&got, "main");
        let feature = branch(&got, "feature");

        assert!(main.head, "HEAD is attached here and nowhere else");
        assert!(!feature.head);
        assert_eq!(main.commit, r.rev_parse("main"));
        assert_eq!(feature.commit, r.rev_parse("feature"));

        // In sync with its upstream, and measured without a process: "" from
        // `%(upstream:track)` *is* the answer when a comparison is possible.
        assert_eq!(
            main.upstream,
            Some(Upstream {
                remote: RefName::from("origin"),
                branch: RefName::from("main"),
                ahead: Some(0),
                behind: Some(0),
            })
        );
        assert!(feature.upstream.is_none(), "no configuration, no claim");
    }

    #[test]
    fn ahead_and_behind_count_commits_each_side_has() {
        let (r, origin) = upstream_fixture("ref-diverged");

        // Two commits only this side has: what a push would send.
        r.write("a.txt", b"a\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "one"]);
        r.write("a.txt", b"b\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "two"]);

        // One commit only the remote has, arrived by fetch: what a pull
        // would bring. A second clone plays the other machine.
        let twin = Scratch::cloned(&origin.0, "ref-diverged-twin");
        twin.write("t.txt", b"t\n");
        twin.git(&["add", "-A"]);
        twin.git(&["commit", "-qm", "theirs"]);
        twin.git(&["push", "-q", "origin", "main"]);
        r.git(&["fetch", "-q", "origin"]);

        let branches = r.open().branches().unwrap();
        let up = branch(&branches, "main")
            .upstream
            .as_ref()
            .expect("still tracking");
        assert_eq!(
            (up.remote.as_bytes(), up.branch.as_bytes()),
            (b"origin".as_slice(), b"main".as_slice())
        );
        assert_eq!(up.ahead, Some(2));
        assert_eq!(up.behind, Some(1), "left and right are not to be confused");
    }

    #[test]
    fn a_gone_upstream_keeps_its_name_but_its_counts_become_unknown() {
        let (r, _) = upstream_fixture("ref-gone");
        // The server deleted the branch and the tracking ref went with it.
        // The branch still remembers what it was tracking — which is worth
        // showing — but nothing can be counted against a ref that is not
        // here.
        r.git(&["update-ref", "-d", "refs/remotes/origin/main"]);

        let branches = r.open().branches().unwrap();
        let up = branch(&branches, "main")
            .upstream
            .as_ref()
            .expect("the configured pair survives the deletion");
        assert_eq!(
            (up.remote.as_bytes(), up.branch.as_bytes()),
            (b"origin".as_slice(), b"main".as_slice())
        );
        assert_eq!(up.ahead, None, "gone is not zero");
        assert_eq!(up.behind, None);
    }

    #[test]
    fn detached_head_is_a_state_and_no_branch_claims_it() {
        let (r, _) = upstream_fixture("ref-detached");
        r.git(&["checkout", "-q", "--detach", "main"]);

        let g = r.open();
        match g.head().unwrap() {
            HeadState::Detached { commit } => assert_eq!(commit, r.rev_parse("HEAD")),
            other => panic!("expected detached, got {other:?}"),
        }
        assert!(
            g.branches().unwrap().iter().all(|b| !b.head),
            "detached means attached to none of them"
        );
    }

    #[test]
    fn a_unicode_branch_name_round_trips_byte_for_byte() {
        // Cyrillic: stable under Unicode normalisation, which matters because
        // loose refs live in filenames and macOS's volume plays games with
        // combining marks. The assertion is on bytes, not text, because the
        // name is addressed back to git byte for byte.
        let r = Scratch::new("ref-unicode");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "seed"]);
        r.git(&["branch", "ветка"]);

        let got = r.open().branches().unwrap();
        let b = branch(&got, "ветка");
        assert_eq!(b.display().as_ref(), "ветка");
        assert_eq!(b.commit, r.rev_parse("ветка"));
    }

    #[test]
    fn remote_branches_name_both_halves_and_skip_the_head_alias() {
        let (r, _origin) = upstream_fixture("ref-remotes");
        // The symbolic alias a clone writes beside the real branches. It
        // points at a branch; it is not one.
        r.git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ]);

        let got = r.open().remote_branches().unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].remote.as_bytes(), b"origin");
        assert_eq!(got[0].branch.as_bytes(), b"main");
        assert_eq!(got[0].commit, r.rev_parse("refs/remotes/origin/main"));
    }

    #[test]
    fn stashes_index_messages_and_commits_newest_first() {
        let r = Scratch::new("ref-stash");
        r.write("seed.txt", b"seed\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "seed"]);

        r.write("seed.txt", b"first\n");
        r.git(&["stash", "push", "-q", "-m", "wip: parser | %gs; \"quoted\""]);
        r.write("seed.txt", b"second\n");
        r.git(&["stash", "push", "-q"]);

        let got = r.open().stashes().unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].index, 0, "newest first, as stash@{{n}} counts");
        assert!(
            !got[0].message.is_empty(),
            "git wrote its own message; it arrives whatever it says"
        );
        assert_eq!(got[1].index, 1);
        // git prefixes its own `On <branch>:` to whatever was given; the
        // model carries the reflog subject as git wrote it.
        assert!(
            got[1].message.ends_with("wip: parser | %gs; \"quoted\""),
            "separators inside a message are content, not structure: {}",
            got[1].message
        );
        assert_eq!(got[0].commit, r.rev_parse("stash@{0}"));
        assert_eq!(got[1].commit, r.rev_parse("stash@{1}"));
    }

    #[test]
    fn remotes_list_each_distinct_url_once() {
        let r = Scratch::new("ref-remotes-model");
        let a = Scratch::bare("ref-remotes-a");
        let b = Scratch::bare("ref-remotes-b");
        let url_a = format!("{}", a.0.display());
        let url_b = format!("{}", b.0.display());
        r.git(&["remote", "add", "origin", &url_a]);
        r.git(&["remote", "add", "upstream", &url_a]);
        // An explicit push address distinct from the fetch address rides
        // beside it; the default one, where fetch and push agree, shows once.
        r.git(&["remote", "set-url", "--add", "--push", "upstream", &url_b]);

        let got = r.open().remotes().unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].name.as_bytes(), b"origin");
        assert_eq!(
            got[0].urls,
            vec![url_a.clone()],
            "same URL both directions, listed once"
        );
        assert_eq!(got[1].name.as_bytes(), b"upstream");
        assert_eq!(got[1].urls, vec![url_a, url_b]);
    }

    #[test]
    fn tags_resolve_to_the_commit_they_name_whichever_kind_they_are() {
        let r = Scratch::new("ref-tags");
        r.write("f.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "one"]);
        r.git(&["tag", "v1"]);
        r.git(&["tag", "-a", "v2", "-m", "release two"]);

        let got = r.open().tags().unwrap();
        let head = r.rev_parse("HEAD");
        assert_eq!(
            branch_names_of_tags(&got),
            vec![b"v1".as_slice(), b"v2".as_slice()],
            "git's own order, by refname"
        );
        let v1 = got.iter().find(|t| t.name.as_bytes() == b"v1").unwrap();
        let v2 = got.iter().find(|t| t.name.as_bytes() == b"v2").unwrap();
        assert_eq!(
            v1.commit, head,
            "lightweight: the object already is the commit"
        );
        assert_eq!(
            v2.commit, head,
            "annotated: peeled past the tag object git created for it"
        );
    }

    fn branch_names_of_tags(tags: &[Tag]) -> Vec<&[u8]> {
        tags.iter().map(|t| t.name.as_bytes()).collect()
    }

    #[test]
    fn reflog_entries_carry_selector_and_message_newest_first_up_to_the_limit() {
        let r = Scratch::new("ref-reflog");
        for i in 0..3 {
            r.write(&format!("f{i}.txt"), format!("{i}\n").as_bytes());
            r.git(&["add", "-A"]);
            r.git(&["commit", "-qm", &format!("number {i}")]);
        }

        let g = r.open();
        let all = g.reflog(5).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].selector, "HEAD@{0}");
        assert_eq!(all[2].selector, "HEAD@{2}");
        assert!(all[0].message.starts_with("commit:"), "{}", all[0].message);

        let log = g.log(1).unwrap();
        assert!(
            log[0].sha.starts_with(&all[0].commit),
            "the abbreviated sha abbreviates the full one"
        );

        assert_eq!(g.reflog(2).unwrap().len(), 2, "the limit bounds the answer");
        assert_eq!(g.reflog(0).unwrap(), vec![], "zero asks for none");
    }

    #[test]
    fn branch_records_parse_positionally_through_empty_fields() {
        // main has no upstream at all — two empty halves between NULs, kept
        // positional. gone has one whose ref left, so track answers by
        // itself. A warning line and a truncated tail are skipped whole,
        // never allowed to shift another record's fields.
        let raw = b"\
            refs/heads/main\0abc\0\0\0\0\0*\n\
            refs/heads/gone\0def\0refs/remotes/origin/main\0origin\0refs/heads/main\0[gone]\0 \n\
            warning: something git wanted to say\n\
            refs/heads/truncated\x00123";

        let got = parse_branches(raw);
        assert_eq!(got.len(), 2, "{got:?}");

        let main = &got[0];
        assert_eq!(main.refname.as_bytes(), b"refs/heads/main");
        assert_eq!(main.commit, "abc");
        assert!(main.head);
        assert!(main.upstream.is_none());

        let gone = &got[1];
        assert!(!gone.head, "a space marks not-HEAD, not a parse failure");
        let up = gone.upstream.as_ref().expect("all three atoms present");
        assert_eq!(up.tracking_ref.as_bytes(), b"refs/remotes/origin/main");
        assert_eq!(up.remote.as_bytes(), b"origin");
        assert_eq!(up.upstream_ref.as_bytes(), b"refs/heads/main");
        assert!(
            matches!(up.track, Track::Gone),
            "the one prose value parsed: it retires the counting process"
        );
    }

    #[test]
    fn stash_records_keep_messages_whatever_they_hold() {
        // Newlines survive inside a message because entries are NUL-framed,
        // not line-split; the trailing NUL leaves a ragged piece that is
        // skipped rather than read as an entry of its own.
        let raw = b"h1\0WIP multi\nline\0h2\0plain\0";
        let got = parse_stashes(raw);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].index, 0);
        assert_eq!(got[0].commit, "h1");
        assert_eq!(got[0].message, "WIP multi\nline");
        assert_eq!(got[1].index, 1);
        assert_eq!(got[1].message, "plain");
    }

    #[test]
    fn tag_records_peel_only_when_a_peel_arrived() {
        let raw = b"\
            refs/tags/v1\0\0aa11\n\
            refs/tags/v2\0bb22\0cc33\n";
        let got = parse_tags(raw);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name.as_bytes(), b"v1");
        assert_eq!(got[0].commit, "aa11", "lightweight: object is commit");
        assert_eq!(got[1].name.as_bytes(), b"v2");
        assert_eq!(got[1].commit, "bb22", "annotated: the peel wins");
    }

    // ------------------------------------------------------------------ writes

    /// A backend that serves only its reads. The verb defaults exist so a
    /// partial implementation — a test fake, a gix port landed read by read —
    /// never has to stub what it does not do.
    struct ReadsOnly;

    impl Repo for ReadsOnly {
        fn log(&self, _: usize) -> Result<Vec<Commit>> {
            Ok(Vec::new())
        }
        fn pairs(&self, _: &str) -> Result<Vec<Pair>> {
            Ok(Vec::new())
        }
        fn status(&self) -> Result<Status> {
            Ok(Status::default())
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }

    #[test]
    fn a_backend_that_does_not_serve_writes_says_so_by_name() {
        assert_eq!(
            ReadsOnly.stage(b"x").unwrap_err(),
            "this repository does not serve staging"
        );
        assert_eq!(
            ReadsOnly.unstage(b"x").unwrap_err(),
            "this repository does not serve unstaging"
        );
        assert_eq!(
            ReadsOnly.discard(b"x").unwrap_err(),
            "this repository does not serve discarding"
        );
        assert_eq!(
            ReadsOnly.remove_untracked(b"x").unwrap_err(),
            "this repository does not serve untracked removal"
        );
        assert_eq!(
            ReadsOnly.ignore(b"x").unwrap_err(),
            "this repository does not serve ignoring"
        );
        // The bulk spellings default to their singulars, so a partial backend
        // that serves one path at a time still answers a stage-all — one
        // unserved word per path rather than per call.
        assert_eq!(
            ReadsOnly.stage_many(&[b"a", b"b"]).unwrap_err(),
            "this repository does not serve staging"
        );
        assert_eq!(
            ReadsOnly.unstage_many(&[b"a"]).unwrap_err(),
            "this repository does not serve unstaging"
        );
        assert_eq!(
            ReadsOnly.commit("hi").unwrap_err(),
            "this repository does not serve committing"
        );
        assert_eq!(
            ReadsOnly.stash_push(None).unwrap_err(),
            "this repository does not serve stashing"
        );
        assert_eq!(
            ReadsOnly.stash_apply(0).unwrap_err(),
            "this repository does not serve applying a stash"
        );
        assert_eq!(
            ReadsOnly.stash_pop(0).unwrap_err(),
            "this repository does not serve popping a stash"
        );
        assert_eq!(
            ReadsOnly.stash_drop(0).unwrap_err(),
            "this repository does not serve dropping a stash"
        );
    }

    #[test]
    fn staging_an_untracked_file_records_it_in_the_index() {
        // The case `git diff` cannot see and the status pass exists for: a
        // brand-new file stages like anything else, because `add` is git's one
        // word for "the index should hold this".
        let r = Scratch::new("stage-untracked");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("new.txt", b"brand new\n");

        let g = r.open();
        assert_eq!(g.status().unwrap().untracked.len(), 1);
        g.stage(b"new.txt").expect("stages");
        let s = g.status().unwrap();
        assert!(s.untracked.is_empty(), "{s:?}");
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].change, Change::Added);
        assert_eq!(s.staged[0].path.as_bytes(), b"new.txt");
    }

    #[test]
    fn staging_addresses_the_path_by_its_bytes() {
        // Spaces plus a Latin-1 byte: the shapes an argv-joined or decoded
        // path would mangle.
        let r = Scratch::new("stage-raw");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        if !r.plant_raw(b"has space \xe9.txt", b"bytes\n") {
            return; // this volume validates UTF-8; see the untracked read test
        }

        let g = r.open();
        g.stage(b"has space \xe9.txt").expect("stages");
        let s = g.status().unwrap();
        assert_eq!(s.staged.len(), 1);
        assert_eq!(
            s.staged[0].path.as_bytes(),
            b"has space \xe9.txt".as_slice(),
            "the index holds the exact name"
        );
        assert!(s.untracked.is_empty());
    }

    #[test]
    fn unstaging_puts_each_change_back_where_it_came_from() {
        let r = Scratch::new("unstage-back");
        r.write("f.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);

        let g = r.open();
        // A modification returns to the working tree's list...
        r.write("f.txt", b"two\n");
        g.stage(b"f.txt").unwrap();
        g.unstage(b"f.txt").expect("unstages");
        let s = g.status().unwrap();
        assert!(s.staged.is_empty());
        assert_eq!(s.unstaged.len(), 1);
        assert!(!s.unstaged[0].submodule.modified, "untouched by the verb");

        // ...and a fresh file returns to untracked, not nowhere.
        r.write("loose.md", b"notes\n");
        g.stage(b"loose.md").unwrap();
        g.unstage(b"loose.md").expect("unstages");
        let s = g.status().unwrap();
        assert!(s.staged.is_empty());
        assert_eq!(
            s.untracked
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>(),
            vec!["loose.md"]
        );
    }

    #[test]
    fn unstaging_nothing_is_a_quiet_no_op() {
        // git answers an unmatched pathspec with success, not an error — and
        // idempotent is the honest answer to "make sure this is unstaged",
        // which is what a toggle key ends up asking twice.
        let r = Scratch::new("unstage-noop");
        r.write("f.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        let g = r.open();
        g.unstage(b"never-seen.txt")
            .expect("no-op on nothing staged");
        g.unstage(b"f.txt").expect("no-op on a clean tracked file");
    }

    #[test]
    fn unstaging_works_on_an_unborn_branch() {
        // Every fresh repository: HEAD names a branch with no commit under it,
        // and the file just staged has to come back out anyway.
        let r = Scratch::new("unstage-unborn");
        r.write("first.txt", b"x\n");
        let g = r.open();
        match g.head().unwrap() {
            HeadState::Branch { commit: None, .. } => {}
            other => panic!("expected unborn, got {other:?}"),
        }
        g.stage(b"first.txt").unwrap();
        g.unstage(b"first.txt").expect("unstages without a HEAD");
        let s = g.status().unwrap();
        assert!(s.staged.is_empty());
        assert_eq!(
            s.untracked
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>(),
            vec!["first.txt"]
        );
    }

    #[test]
    fn discarding_a_modified_file_restores_what_the_index_holds() {
        let r = Scratch::new("discard-modified");
        r.write("f.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("f.txt", b"ruined\n");

        let g = r.open();
        assert_eq!(g.status().unwrap().unstaged.len(), 1);
        g.discard(b"f.txt").expect("discards");
        let s = g.status().unwrap();
        assert!(s.is_empty(), "the working tree came back clean: {s:?}");
        assert_eq!(std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(), b"one\n");
    }

    #[test]
    fn a_staged_version_survives_a_discard_of_the_worktree_side() {
        // The line `checkout --` draws: index against worktree. Edited,
        // staged, edited again — discarding takes the second edit only, and
        // the first stays in the index exactly as stage left it.
        let r = Scratch::new("discard-staged");
        r.write("f.txt", b"original\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("f.txt", b"staged version\n");
        r.git(&["add", "f.txt"]);
        r.write("f.txt", b"staged version\nand more\n");

        let g = r.open();
        g.discard(b"f.txt").expect("discards");
        let s = g.status().unwrap();
        assert!(s.unstaged.is_empty(), "{s:?}");
        assert_eq!(s.staged.len(), 1, "the index kept its copy");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            b"staged version\n",
            "the worktree went back to the index, not to HEAD"
        );
    }

    #[test]
    fn discarding_addresses_the_path_by_its_bytes() {
        // Spaces plus a Latin-1 byte, modified then discarded through the
        // exact name status reported — the same discipline staging keeps.
        let r = Scratch::new("discard-raw");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        if !r.plant_raw(b"has space \xe9.txt", b"before\n") {
            return; // this volume validates UTF-8; see the staging read test
        }
        // Tracked on purpose: `discard` is `checkout --`, and checkout restores
        // what the index holds. An untracked name is `remove_untracked`'s
        // caller's decision — a different destruction with no undo.
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "latin-1"]);

        let g = r.open();
        r.plant_raw(b"has space \xe9.txt", b"after\n");
        g.discard(b"has space \xe9.txt").expect("discards");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"has space \xe9.txt")).unwrap(),
            b"before\n",
            "restored by the real bytes, not a decoded near-miss"
        );
    }

    #[test]
    fn an_untracked_file_is_deleted_by_its_own_verb() {
        // Discard's other mechanics: nothing to check out, so the file just
        // stops existing — off the disk and out of status with one call.
        let r = Scratch::new("discard-untracked");

        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("notes.md", b"draft\n");

        let g = r.open();
        assert_eq!(g.status().unwrap().untracked.len(), 1);
        g.remove_untracked(b"notes.md").expect("deletes");
        let s = g.status().unwrap();
        assert!(s.is_empty(), "{s:?}");
        assert!(!join_raw(&r.0, b"notes.md").exists());
    }

    /// The HEAD→worktree diff of `path`, through the same free function
    /// acquisition uses — so a synthesized patch is tested against exactly
    /// the shape the view would hand over, hunks and all.
    fn diff_files(g: &Handle) -> Vec<gitten_core::FileDiff> {
        let differs = gitten_core::differ::Differs::builtin();
        crate::diff(g.as_ref(), "", &differs, &Default::default()).expect("diffs")
    }

    #[test]
    fn staging_a_synthesized_hunk_lands_exactly_its_lines_in_the_index() {
        // Sixteen lines, two edits six-plus lines apart: two hunks under the
        // default context. Everything here runs against REAL git apply — the
        // synthesis golden tests in core prove the bytes; these prove the
        // bytes are the ones git accepts and aims correctly.
        let r = Scratch::new("hunk-stage");
        let base: String = (1..=16).map(|i| format!("line-{i:02}\n")).collect();
        r.write("f.txt", base.as_bytes());
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        let edited = base
            .replace("line-02\n", "EDIT-TWO\n")
            .replace("line-14\n", "EDIT-FOURTEEN\n");
        r.write("f.txt", edited.as_bytes());

        let g = r.open();
        let files = diff_files(&g);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks.len(), 2, "the fixture holds two hunks");

        // Stage the first hunk alone.
        let patch = gitten_core::patch::emit(&files[0].path, &[&files[0].hunks[0]]);
        assert!(!patch.is_empty());
        g.stage_patch(&patch)
            .expect("git apply --cached takes the hunk");

        // The index holds edit one only; the worktree still holds both —
        // which is exactly what makes status say MM.
        let porcelain = || -> String {
            String::from_utf8_lossy(
                &r.cmd(&["status".into(), "--porcelain".into()])
                    .output()
                    .expect("status")
                    .stdout,
            )
            .into_owned()
        };
        assert_eq!(
            porcelain(),
            "MM f.txt\n",
            "staged AND unstaged entries for one file"
        );
        let indexed = r
            .cmd(&["show".into(), ":f.txt".into()])
            .output()
            .expect("cat-file");
        assert!(indexed.status.success());
        let indexed = String::from_utf8_lossy(&indexed.stdout).into_owned();
        assert!(indexed.contains("EDIT-TWO\n"), "hunk one is in");
        assert!(indexed.contains("line-14\n"), "hunk two is not");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            edited.as_bytes(),
            "the working tree was never touched by --cached"
        );

        // And running it backwards puts the index back where it was — the
        // unstage direction of the same text.
        g.unstage_patch(&patch).expect("reverses cleanly");
        assert_eq!(porcelain(), " M f.txt\n", "back to unstaged-only");
        let indexed = String::from_utf8_lossy(
            &r.cmd(&["rev-parse".into(), ":f.txt".into()])
                .output()
                .expect("rev-parse :f.txt")
                .stdout,
        )
        .into_owned();
        let head = String::from_utf8_lossy(
            &r.cmd(&["rev-parse".into(), "HEAD:f.txt".into()])
                .output()
                .expect("rev-parse HEAD:f.txt")
                .stdout,
        )
        .into_owned();
        assert_eq!(
            indexed.trim(),
            head.trim(),
            "unstaging restored the index to HEAD exactly"
        );
    }

    #[test]
    fn discarding_a_synthesized_hunk_takes_only_its_lines_from_the_worktree() {
        let r = Scratch::new("hunk-discard");
        let base: String = (1..=16).map(|i| format!("line-{i:02}\n")).collect();
        r.write("f.txt", base.as_bytes());
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        let edited = base
            .replace("line-02\n", "EDIT-TWO\n")
            .replace("line-14\n", "EDIT-FOURTEEN\n");
        r.write("f.txt", edited.as_bytes());

        let g = r.open();
        let files = diff_files(&g);
        let patch = gitten_core::patch::emit(&files[0].path, &[&files[0].hunks[1]]);

        // DESTRUCTIVE in the view; here it simply runs.
        g.discard_patch(&patch).expect("reverses onto the worktree");

        let now = String::from_utf8(std::fs::read(join_raw(&r.0, b"f.txt")).unwrap()).unwrap();
        assert!(now.contains("line-14\n"), "hunk two's line came back");
        assert!(
            now.contains("EDIT-TWO\n"),
            "hunk one's edit survives — discard aimed at one hunk"
        );
        let porcelain = String::from_utf8_lossy(
            &r.cmd(&["status".into(), "--porcelain".into()])
                .output()
                .expect("status")
                .stdout,
        )
        .into_owned();
        assert_eq!(porcelain, " M f.txt\n", "nothing staged by discarding");
    }

    #[test]
    fn a_drifted_index_refuses_the_patch_in_gits_own_words() {
        // Stage hunk one, then try to stage it again: the lines the patch
        // removes are no longer in the index, and git says so. That sentence
        // — not a paraphrase — is what the error band owes the reader.
        let r = Scratch::new("hunk-drift");
        let base: String = (1..=16).map(|i| format!("line-{i:02}\n")).collect();
        r.write("f.txt", base.as_bytes());
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("f.txt", base.replace("line-02\n", "EDIT-TWO\n").as_bytes());

        let g = r.open();
        let files = diff_files(&g);
        let patch = gitten_core::patch::emit(&files[0].path, &[&files[0].hunks[0]]);
        g.stage_patch(&patch).expect("first apply lands");

        let err = g.stage_patch(&patch).expect_err("second apply refuses");
        assert!(
            err.contains("patch failed") || err.contains("does not apply"),
            "{err}"
        );
    }

    #[test]
    fn an_empty_patch_is_refused_before_anything_runs() {
        let r = Scratch::new("hunk-empty");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        let g = r.open();
        for verb in ["stage", "unstage", "discard"] {
            let err = match verb {
                "stage" => g.stage_patch(b""),
                "unstage" => g.unstage_patch(b""),
                _ => g.discard_patch(b""),
            }
            .expect_err("empty refuses");
            assert!(err.contains("empty patch"), "{verb}: {err}");
        }
    }

    #[test]
    fn a_deletion_synthesizes_and_applies_in_both_directions() {
        // The /dev/null shape that needs no mode of its own: a deleted file
        // stages as a deletion — the index already knows the mode — and the
        // same text reversed lets go again.
        let r = Scratch::new("hunk-null-side");
        let base: String = (1..=8).map(|i| format!("line-{i:02}\n")).collect();
        r.write("kept.txt", base.as_bytes());
        r.write("doomed.txt", base.as_bytes());
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        std::fs::remove_file(join_raw(&r.0, b"doomed.txt")).expect("rm doomed");

        let g = r.open();
        let files = diff_files(&g);
        let doomed = files
            .iter()
            .find(|f| f.path == "doomed.txt")
            .expect("deletion");
        assert!(!doomed.hunks.is_empty());
        let patch = gitten_core::patch::emit(&doomed.path, &[&doomed.hunks[0]]);

        g.stage_patch(&patch).expect("deletion stages");
        let porcelain = || {
            String::from_utf8_lossy(
                &r.cmd(&["status".into(), "--porcelain".into()])
                    .output()
                    .expect("status")
                    .stdout,
            )
            .into_owned()
        };
        assert!(porcelain().contains("D  doomed.txt"), "{}", porcelain());

        // The reverse direction on real apply: the index lets the deletion
        // go and the path is simply unstaged work again.
        g.unstage_patch(&patch).expect("deletion reverses");
        assert!(porcelain().contains(" D doomed.txt"), "{}", porcelain());
    }

    #[test]
    fn staging_a_creation_is_gits_refusal_and_it_names_the_missing_side() {
        // The documented limit: a pure-addition patch has no mode to create
        // the entry with, so `apply --cached` refuses. The shell refuses
        // before here; this pins what surfaces if anything ever sends one.
        let r = Scratch::new("hunk-creation");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("fresh.txt", b"a\nb\nc\n");

        let g = r.open();
        let files = diff_files(&g);
        let fresh = files
            .iter()
            .find(|f| f.path == "fresh.txt")
            .expect("loose pair");
        let patch = gitten_core::patch::emit(&fresh.path, &[&fresh.hunks[0]]);
        let err = g.stage_patch(&patch).expect_err("creation is refused");
        assert!(err.contains("does not exist in index"), "{err}");
        // And nothing landed: the refusal left no half-state behind.
        let s = g.status().unwrap();
        assert_eq!(s.untracked.len(), 1);
        assert!(s.staged.is_empty(), "{s:?}");
    }

    #[test]
    fn a_missing_final_newline_refuses_every_direction_and_moves_nothing() {
        // The documented limit, pinned: the line model cannot say "this file
        // has no final newline", so a hunk touching its last line goes out
        // claiming one. git must REFUSE that — refusing is honest; applying
        // it would add or eat the terminator byte-for-byte. All three verbs
        // are proven to refuse AND to leave both sides untouched, because
        // the directions differ in what they read: `--cached` reads the
        // index blob, and the worktree reverse writes the file itself.
        let r = Scratch::new("hunk-no-newline");
        let base = b"alpha\nbeta\ngamma";
        r.write("f.txt", base);
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        let edited = b"alpha\nbeta\nGAMMA";
        r.write("f.txt", edited);

        let g = r.open();
        let files = diff_files(&g);
        let patch = gitten_core::patch::emit(&files[0].path, &[&files[0].hunks[0]]);

        let index_is_head = || {
            String::from_utf8_lossy(
                &r.cmd(&["rev-parse".into(), ":f.txt".into()])
                    .output()
                    .expect("rev-parse :f.txt")
                    .stdout,
            ) == String::from_utf8_lossy(
                &r.cmd(&["rev-parse".into(), "HEAD:f.txt".into()])
                    .output()
                    .expect("rev-parse HEAD:f.txt")
                    .stdout,
            )
        };
        let worktree = || std::fs::read(join_raw(&r.0, b"f.txt")).unwrap();

        for (verb, err) in [
            ("stage", g.stage_patch(&patch)),
            ("discard", g.discard_patch(&patch)),
            ("unstage", g.unstage_patch(&patch)),
        ] {
            let err = err.expect_err("git refuses a patch that lies about EOF");
            // Verbatim, both lines: where git tripped and its verdict. A
            // paraphrase here would also match a misapplication that merely
            // warned; this sentence exists only on a refusal.
            assert!(
                err.contains("error: patch failed: f.txt:1")
                    && err.contains("error: f.txt: patch does not apply"),
                "{verb}: {err}"
            );
            assert!(index_is_head(), "{verb}: the index moved on a refusal");
            assert_eq!(worktree(), edited.to_vec(), "{verb}: the worktree moved");
        }
        assert_ne!(*base, *edited, "the fixture really changed the last line");
    }

    #[test]
    fn the_patch_verbs_reach_the_trait_through_the_same_stdin_transport() {
        // A recording backend answers success; the point is the plumbing —
        // bytes in, no path arguments anywhere.
        use std::sync::Mutex;
        struct Patches(Mutex<Vec<Vec<u8>>>);
        impl Repo for Patches {
            fn log(&self, _: usize) -> Result<Vec<Commit>> {
                Ok(Vec::new())
            }
            fn pairs(&self, _: &str) -> Result<Vec<Pair>> {
                Ok(Vec::new())
            }
            fn status(&self) -> Result<gitten_core::status::Status> {
                Ok(Default::default())
            }
            fn describe(&self) -> String {
                "patches".into()
            }
            fn stage_patch(&self, p: &[u8]) -> Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push([b"s".to_vec(), p.to_vec()].concat());
                Ok(())
            }
            fn unstage_patch(&self, p: &[u8]) -> Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push([b"u".to_vec(), p.to_vec()].concat());
                Ok(())
            }
            fn discard_patch(&self, p: &[u8]) -> Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push([b"d".to_vec(), p.to_vec()].concat());
                Ok(())
            }
        }
        let patches = Arc::new(Patches(Mutex::new(Vec::new())));
        let g: Handle = Arc::clone(&patches) as Handle;
        g.discard_patch(b"-- hunk\n").expect("discard reaches");
        assert_eq!(
            *patches.0.lock().unwrap(),
            vec![b"d-- hunk\n".to_vec()],
            "the bytes arrived whole"
        );
    }

    #[test]
    fn removing_an_untracked_path_may_not_leave_the_repository() {
        // The fence around the only verb that unlinks directly: absolute
        // names would have `join` replace the root outright, and `..` would
        // walk out past a lexical prefix check. Both refuse in words, and
        // what they name survives.
        let r = Scratch::new("untracked-fence");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);

        // A real file just outside the root, at exactly where `..` lands.
        let outside = r.0.parent().unwrap().join("flee-escape-test.txt");
        std::fs::write(&outside, b"still here\n").expect("a file beside the repo");
        let outside_name = format!("../{}", outside.file_name().unwrap().to_string_lossy());

        let g = r.open();
        let absolute = g.remove_untracked(b"/etc/passwd").unwrap_err();
        assert!(
            absolute.contains("not inside this repository"),
            "{absolute}"
        );
        let relative = g.remove_untracked(outside_name.as_bytes()).unwrap_err();
        assert!(
            relative.contains("not inside this repository"),
            "{relative}"
        );
        assert!(outside.exists(), "the neighbour file was not touched");
        let _ = std::fs::remove_file(&outside);

        // And the fence is on the deletion only: git itself governs where a
        // pathspec may point, so discard needs no second opinion.
        g.discard(b"seed.txt").expect("inside the fence, as always");
    }

    #[test]
    fn bulk_stage_and_unstage_take_every_path_in_one_call() {
        let r = Scratch::new("bulk");
        r.write("a.txt", b"a\n");
        r.write("b.txt", b"b\n");
        r.write("c.txt", b"c\n");
        let g = r.open();

        // Stage-all on a fresh tree: everything lands in the index at once,
        // including the untracked files `git diff` cannot see.
        g.stage_many(&[b"a.txt", b"b.txt", b"c.txt"])
            .expect("stages all");
        let s = g.status().unwrap();
        assert!(s.untracked.is_empty() && s.staged.len() == 3, "{s:?}");

        // And the mirror: every path back out of the index, one process.
        g.unstage_many(&[b"a.txt", b"b.txt", b"c.txt"])
            .expect("unstages all");
        let s = g.status().unwrap();
        assert!(s.staged.is_empty() && s.untracked.len() == 3, "{s:?}");
    }

    #[test]
    fn argv_chunks_take_at_least_one_path_and_never_cross_the_budget() {
        let small: &[&[u8]] = &[b"a", b"b", b"c"];
        assert_eq!(
            chunk_end(small, 0),
            3,
            "a list under the budget is one chunk"
        );

        // A single name larger than the budget still travels: progress
        // beats deadlock, because a chunk of zero would loop forever.
        let big = vec![b'x'; ARGV_BUDGET + 1];
        let one: [&[u8]; 1] = [&big];
        assert_eq!(chunk_end(&one, 0), 1);

        // And the boundary itself: fill up to the budget, stop before the
        // path that would cross it.
        let half = vec![b'y'; ARGV_BUDGET / 2 + 1];
        let pair: [&[u8]; 2] = [&half, &half];
        assert_eq!(
            chunk_end(&pair, 0),
            1,
            "the second half would cross the budget"
        );
        assert_eq!(chunk_end(&pair, 1), 2);
    }

    #[test]
    fn staging_a_tree_too_big_for_one_argv_still_stages_everything() {
        // ~900 paths at ~124 bytes each is ~110 KB of argv — past
        // [`ARGV_BUDGET`], so the verb runs more than one `add`. The whole
        // point: stage-all's own use case must not be the thing that breaks
        // it with E2BIG. The assertion nobody notices is the point.
        let r = Scratch::new("bulk-chunks");
        r.write(".gitignore", b"");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);

        let paths: Vec<String> = (0..900)
            .map(|i| format!("bulk/{i:04}/{}.txt", "p".repeat(110)))
            .collect();
        for name in &paths {
            r.write(name, b"\n");
        }
        let refs: Vec<&[u8]> = paths.iter().map(|p| p.as_bytes()).collect();

        let g = r.open();
        g.stage_many(&refs).expect("stages across every chunk");
        let s = g.status().unwrap();
        assert_eq!(
            s.staged.len(),
            paths.len(),
            "{:?} summaries lie",
            s.staged.len()
        );
        assert!(s.untracked.is_empty(), "{s:?}");

        // And back out again, through however many resets it takes.
        g.unstage_many(&refs).expect("unstages across every chunk");
        let s = g.status().unwrap();
        assert!(s.staged.is_empty());
        assert_eq!(s.untracked.len(), paths.len());
    }

    #[test]
    fn ignoring_creates_the_gitignore_and_git_stops_listing_the_file() {
        // The whole honest chain: the file stays on disk, untracked but now
        // ignored — the entry leaves status because git itself stops listing
        // ignored files, not because anything moved.
        let r = Scratch::new("ignore-create");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("log.txt", b"noise\n");

        let g = r.open();
        g.ignore(b"log.txt").expect("ignores");

        assert_eq!(
            std::fs::read(join_raw(&r.0, b".gitignore")).unwrap(),
            b"/log.txt\n",
            "anchored at the root, one line, terminated"
        );
        assert!(join_raw(&r.0, b"log.txt").exists(), "nothing was deleted");
        let s = g.status().unwrap();
        // And .gitignore itself is now the tree's one untracked file — the
        // user commits it or not, as they like.
        assert_eq!(
            s.untracked
                .iter()
                .map(|e| e.path.to_string())
                .collect::<Vec<_>>(),
            vec![".gitignore"],
            "{s:?}"
        );
    }

    #[test]
    fn ignoring_survives_a_gitignore_without_a_trailing_newline() {
        // The ordinary state of a hand-edited .gitignore: no final newline.
        // Gluing onto it would silently extend somebody's last pattern.
        let r = Scratch::new("ignore-newline");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write(".gitignore", b"build/");

        r.open().ignore(b"x.txt").expect("ignores");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b".gitignore")).unwrap(),
            b"build/\n/x.txt\n",
            "the new line started on its own"
        );
    }

    #[test]
    fn ignoring_twice_writes_one_line() {
        let r = Scratch::new("ignore-idempotent");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);

        let g = r.open();
        g.ignore(b"junk.bin").unwrap();
        let before = std::fs::read(join_raw(&r.0, b".gitignore")).unwrap();
        g.ignore(b"junk.bin").unwrap();
        assert_eq!(
            std::fs::read(join_raw(&r.0, b".gitignore")).unwrap(),
            before,
            "the second ask changed nothing"
        );
    }

    #[test]
    fn ignoring_escapes_what_git_would_read_as_a_pattern() {
        // `weird[name]?.txt` is a filename, not a character class — and the
        // proof is git's own answer: after ignoring, status stops listing it,
        // which only happens if the written pattern matches the real name.
        // A committed empty .gitignore keeps the tree's untracked list at
        // exactly the file under test.
        let r = Scratch::new("ignore-glob");
        r.write(".gitignore", b"");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        if !r.plant_raw(b"weird[name]?.txt", b"oddly named\n") {
            return; // this volume validates UTF-8 and refused the name
        }

        let g = r.open();
        g.ignore(b"weird[name]?.txt").expect("ignores");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b".gitignore")).unwrap(),
            b"/weird\\[name\\]\\?.txt\n",
            "every glob character made literal"
        );
        assert!(g.status().unwrap().untracked.is_empty());
    }

    #[test]
    fn a_name_the_line_cannot_carry_rides_nowhere() {
        // Removed: an earlier draft spelled unrepresentable names C-quoted,
        // the way git prints them — and git's own matcher does not read
        // quotes back in .gitignore. `check-ignore` said no for every
        // spelling tried, which is why refusal replaced it.
        let r = Scratch::new("ignore-backslash");
        r.write(".gitignore", b"");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        if !r.plant_raw(b"back\\slash.txt", b"\n") {
            return; // refused by this volume; the escape table is pinned above
        }

        let g = r.open();
        assert_eq!(g.status().unwrap().untracked.len(), 1);
        g.ignore(b"back\\slash.txt").expect("ignores");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b".gitignore")).unwrap(),
            b"/back\\\\slash.txt\n",
            "the name's own backslash escaped, not quoted around"
        );
        assert!(g.status().unwrap().untracked.is_empty());
    }

    #[test]
    fn ignoring_a_path_with_spaces_and_non_utf8_bytes_lands_byte_exact() {
        let r = Scratch::new("ignore-raw");
        r.write(".gitignore", b"");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        if !r.plant_raw(b"caf\xe9 notes.txt", b"\n") {
            return; // this volume validates UTF-8; see the staging read test
        }

        let g = r.open();
        g.ignore(b"caf\xe9 notes.txt").expect("ignores");
        // High bytes pass through raw — quoting is about the grammar, not
        // the encoding — while the space needs nothing at all mid-line.
        assert_eq!(
            std::fs::read(join_raw(&r.0, b".gitignore")).unwrap(),
            b"/caf\xe9 notes.txt\n"
        );
        assert!(g.status().unwrap().untracked.is_empty());
    }

    #[test]
    fn ignore_lines_are_anchored_escaped_and_refuse_only_the_unspellable() {
        // The plain case: anchored, nothing else.
        assert_eq!(ignore_line(b"log.txt"), Some(b"/log.txt".to_vec()));
        // A leading `!` or `#` loses its special meaning to the anchor alone.
        assert_eq!(ignore_line(b"!not.txt"), Some(b"/!not.txt".to_vec()));
        assert_eq!(ignore_line(b"#notes.md"), Some(b"/#notes.md".to_vec()));
        // Glob characters are literal wherever they occur.
        assert_eq!(
            ignore_line(b"a*b?[c]d"),
            Some(b"/a\\*b\\?\\[c\\]d".to_vec())
        );
        // A name's own backslash escapes like any glob character — checked
        // against git's own matcher by the scratch test beside this one.
        assert_eq!(
            ignore_line(b"back\\slash"),
            Some(b"/back\\\\slash".to_vec())
        );
        // Quotes and tabs mid-line are ordinary bytes to git; nothing is
        // escaped that does not need it.
        assert_eq!(ignore_line(b"say \"hi\""), Some(b"/say \"hi\"".to_vec()));
        assert_eq!(ignore_line(b"tab\there"), Some(b"/tab\there".to_vec()));
        // Trailing spaces survive only escaped, every one of them.
        assert_eq!(ignore_line(b"end .txt "), Some(b"/end .txt\\ ".to_vec()));
        assert_eq!(ignore_line(b"two  "), Some(b"/two\\ \\ ".to_vec()));
        // High bytes are not grammar: they pass through raw.
        assert_eq!(ignore_line(b"caf\xe9"), Some(b"/caf\xe9".to_vec()));
        // A line break has no spelling — patterns are read one line at a
        // time — so the honest answer is refusal, not a line that matches
        // nothing. Checked against the binary below, not assumed here.
        assert_eq!(ignore_line(b"new\nline"), None);
        assert_eq!(ignore_line(b"old\rline"), None);
    }

    #[test]
    fn a_name_with_a_line_break_is_refused_in_words_and_the_tree_is_untouched() {
        let r = Scratch::new("refused");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        if !r.plant_raw(b"two\nlines.txt", b"\n") {
            return;
        }

        let g = r.open();
        let e = g.ignore(b"two\nlines.txt").unwrap_err();
        assert!(e.contains("cannot be ignored"), "{e}");
        assert!(!join_raw(&r.0, b".gitignore").exists(), "nothing written");
        assert_eq!(
            g.status().unwrap().untracked.len(),
            1,
            "the file still shows, as it must until it can be matched"
        );
    }

    #[test]
    fn the_first_commit_lands_on_an_unborn_branch_and_matches_git() {
        let r = Scratch::new("commit-unborn");
        r.write("f.txt", b"x\n");
        let g = r.open();
        g.stage(b"f.txt").expect("stages");
        let sha = g.commit("first\n\nwith a body").expect("commits");

        // The returned OID is git's own answer, not one we computed.
        assert_eq!(sha, r.rev_parse("HEAD"));
        match g.head().unwrap() {
            HeadState::Branch { name, commit } => {
                assert_eq!(name.as_bytes(), b"main", "the branch was born");
                assert_eq!(commit.as_deref(), Some(sha.as_str()));
            }
            other => panic!("{other:?}"),
        }
        // And the subject is the message's first line, as log will show it.
        assert_eq!(g.log(1).unwrap()[0].subject, "first");
    }

    #[test]
    fn the_message_reaches_the_commit_whatever_it_holds() {
        // Quotes, ampersands, newlines, non-ASCII, no trailing newline — every
        // shape that breaks a message passed as an argv word.
        let r = Scratch::new("commit-message");
        r.write("f.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("f.txt", b"y\n");
        r.git(&["add", "f.txt"]);

        let message =
            "he said \"stage it\" & left <>\n\nbody with é, 中文, 🎯\nand \"quotes\" again";
        let sha = r.open().commit(message).expect("commits");
        assert_eq!(sha, r.rev_parse("HEAD"));

        // The stored bytes, read back through git itself. git appends the one
        // final newline it promises every commit; everything else is verbatim.
        let raw = String::from_utf8(r.git_os_out(&[
            "show".into(),
            "-s".into(),
            "--format=%B".into(),
            sha.into(),
        ]))
        .unwrap();
        assert_eq!(raw.trim_end_matches('\n'), message);
    }

    #[test]
    fn an_empty_message_is_refused_without_touching_history_or_index() {
        let r = Scratch::new("commit-empty");
        r.write("f.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("f.txt", b"y\n");

        let g = r.open();
        g.stage(b"f.txt").unwrap();
        for empty in ["", "   ", "\n\t \n"] {
            let e = g.commit(empty).unwrap_err();
            assert!(e.contains("message"), "{empty:?}: {e}");
        }
        assert_eq!(g.log(5).unwrap().len(), 1, "nothing was committed");
        assert_eq!(
            g.status().unwrap().staged.len(),
            1,
            "the index kept its entry"
        );
    }

    #[test]
    fn a_failing_commit_reports_gits_own_words() {
        // No user identity configured in this shell of a repository beyond the
        // scratch defaults is NOT the failure here — instead, make git fail
        // with nothing staged, where its own diagnosis is the useful answer.
        let r = Scratch::new("commit-fails");
        r.write("seed.txt", b"x\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        let e = r.open().commit("nothing staged").unwrap_err();
        assert!(e.starts_with("git commit:"), "{e}");
        assert!(!e.trim().is_empty(), "git's stderr travelled");
    }

    // ------------------------------------------------------- the branch verbs

    /// Two commits on `main` and a `feature` branch pinned to the first, with
    /// a file that differs between them — the least state checkout, dirty-tree
    /// refusal and unmerged deletion mean anything in.
    fn two_branches(name: &str) -> Scratch {
        let r = Scratch::new(name);
        r.write("shared.txt", b"on main\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "first"]);
        r.git(&["branch", "feature"]);
        r.write("shared.txt", b"on main, edited\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "second"]);
        r
    }

    // ------------------------------------------------------ the stash verbs

    /// A repository with one tracked modification in the working tree: the
    /// least a push has to chew on.
    fn with_dirty_tree(name: &str) -> Scratch {
        let r = Scratch::new(name);
        r.write("f.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("f.txt", b"one\ntwo\n");
        r
    }

    #[test]
    fn checkout_moves_head_and_the_working_tree_together() {
        let r = two_branches("checkout");
        let g = r.open();

        // Where feature points, asked while main can still answer.
        let feature_at = r.rev_parse("HEAD~1");

        assert_eq!(g.checkout(b"feature").map(|_| ()), Ok(()));
        match g.head().unwrap() {
            HeadState::Branch { name, commit } => {
                assert_eq!(name.as_bytes(), b"feature");
                assert_eq!(commit.as_deref(), Some(feature_at.as_str()));
            }
            other => panic!("attached HEAD expected, got {other:?}"),
        }
        // The tree followed: what is on disk is what feature says.
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"shared.txt")).unwrap(),
            b"on main\n"
        );

        // And back. The round trip is the part a one-way test can miss.
        g.checkout(b"main").unwrap();
        match g.head().unwrap() {
            HeadState::Branch { name, .. } => assert_eq!(name.as_bytes(), b"main"),
            other => panic!("attached HEAD expected, got {other:?}"),
        }
    }

    #[test]
    fn checkout_reaches_a_branch_from_detached_head() {
        // Detached is where half of all checkouts *start* — a bisect, an old
        // release — so the state the verb has to be able to leave is the one
        // this repo's own panel shows first.
        let r = two_branches("checkout-detached");
        let g = r.open();
        r.git(&["checkout", "-q", "--detach", "HEAD~1"]);
        assert!(matches!(g.head().unwrap(), HeadState::Detached { .. }));

        g.checkout(b"main").unwrap();
        match g.head().unwrap() {
            HeadState::Branch { name, commit } => {
                assert_eq!(name.as_bytes(), b"main");
                assert_eq!(commit.as_deref(), Some(r.rev_parse("main").as_str()));
            }
            other => panic!("attached HEAD expected, got {other:?}"),
        }
    }

    #[test]
    fn a_checkout_that_would_lose_work_refuses_in_gits_own_words() {
        let r = two_branches("checkout-dirty");
        let g = r.open();
        g.checkout(b"feature").unwrap();
        // A change on top of feature that main cannot carry: exactly the shape
        // git stops to warn about.
        r.write("shared.txt", b"uncommitted work\n");

        let e = g.checkout(b"main").unwrap_err();
        assert!(
            e.contains("local changes") && e.contains("overwritten"),
            "git's own refusal came through verbatim: {e}"
        );
        // And nothing moved — the refusal was a stop, not a detour.
        match g.head().unwrap() {
            HeadState::Branch { name, .. } => assert_eq!(name.as_bytes(), b"feature"),
            other => panic!("HEAD stayed put, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"shared.txt")).unwrap(),
            b"uncommitted work\n",
            "the working tree kept the uncommitted work"
        );
    }

    #[test]
    fn a_push_parks_the_tracked_changes_and_the_tree_goes_clean() {
        let r = with_dirty_tree("stash-push");
        let g = r.open();

        assert_eq!(
            g.stash_push(None).expect("pushes"),
            0,
            "the new entry is top"
        );
        assert_eq!(g.stashes().unwrap().len(), 1);
        assert!(g.stashes().unwrap()[0].message.starts_with("WIP on main"));
        // The point of the verb, as the files pane will show it: the parked
        // work leaves every list, staged and unstaged alike.
        let s = g.status().unwrap();
        assert!(s.staged.is_empty() && s.unstaged.is_empty(), "{s:?}");

        // And pop brings the work back, spending the entry.
        g.stash_pop(0).expect("pops");
        assert!(g.stashes().unwrap().is_empty());
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            b"one\ntwo\n",
            "the working tree holds what was parked"
        );
    }

    #[test]
    fn a_push_can_carry_a_message() {
        let r = with_dirty_tree("stash-message");
        let g = r.open();
        g.stash_push(Some("hand written")).expect("pushes");
        assert!(
            g.stashes().unwrap()[0].message.ends_with("hand written"),
            "{}",
            g.stashes().unwrap()[0].message
        );
    }

    #[test]
    fn a_branch_is_created_where_it_is_told_and_at_head_when_not() {
        let r = two_branches("branch-create");
        let g = r.open();

        let start = r.rev_parse("HEAD~1");
        g.create_branch(b"pinned", Some(start.as_bytes())).unwrap();
        g.create_branch(b"here", None).unwrap();

        let branches = g.branches().unwrap();
        let at = |name: &str| branch(&branches, name).commit.clone();
        assert_eq!(at("pinned"), start, "the start point was honoured");
        assert_eq!(at("here"), r.rev_parse("HEAD"), "default is HEAD");
        // Creating checked nothing out.
        match g.head().unwrap() {
            HeadState::Branch { name, .. } => assert_eq!(name.as_bytes(), b"main"),
            other => panic!("HEAD unmoved, got {other:?}"),
        }

        // A second branch by the same name is git's error, quoted.
        let e = g.create_branch(b"pinned", None).unwrap_err();
        assert!(e.contains("already exists"), "{e}");
    }

    #[test]
    fn an_unnamed_branch_is_refused_before_git_sees_it() {
        let r = two_branches("branch-nameless");
        let g = r.open();
        for empty in [&b""[..], &b"  \t "[..]] {
            let e = g.create_branch(empty, None).unwrap_err();
            assert!(e.contains("name"), "{empty:?}: {e}");
            let e = g.rename_branch(b"main", empty).unwrap_err();
            assert!(e.contains("name"), "{empty:?}: {e}");
        }
        // Nothing landed in either direction.
        assert_eq!(g.branches().unwrap().len(), 2);
    }

    #[test]
    fn deleting_an_unmerged_branch_needs_force_and_force_answers_it() {
        let r = two_branches("branch-delete");
        let g = r.open();
        // feature holds the first commit only; give it a commit main lacks so
        // `-d` has something real to refuse.
        g.checkout(b"feature").unwrap();
        r.write("shared.txt", b"on feature\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "feature work"]);
        g.checkout(b"main").unwrap();

        // First press: git's safety, verbatim.
        let e = g.delete_branch(b"feature", false).unwrap_err();
        assert!(
            e.contains("not fully merged"),
            "git's own refusal came through: {e}"
        );
        assert!(
            g.branches()
                .unwrap()
                .iter()
                .any(|b| b.name.as_bytes() == b"feature"),
            "the branch survived"
        );

        // Force is the reader's answer to exactly that sentence.
        g.delete_branch(b"feature", true).unwrap();
        let names: Vec<Vec<u8>> = g
            .branches()
            .unwrap()
            .iter()
            .map(|b| b.name.as_bytes().to_vec())
            .collect();
        assert_eq!(names, vec![b"main".to_vec()]);
    }

    #[test]
    fn renaming_a_branch_moves_everything_gits_m_would_move() {
        let (r, _origin) = upstream_fixture("branch-rename");
        // `main` tracks `origin/main`; the rename has to carry that link.
        let g = r.open();
        g.rename_branch(b"main", b"trunk").unwrap();

        let names: Vec<Vec<u8>> = g
            .branches()
            .unwrap()
            .iter()
            .map(|b| b.name.as_bytes().to_vec())
            .collect();
        assert_eq!(names, vec![b"trunk".to_vec()]);
        let branches = g.branches().unwrap();
        let trunk = &branches[0];
        let upstream = trunk
            .upstream
            .as_ref()
            .expect("the tracking link moved too");
        assert_eq!(upstream.branch.as_bytes(), b"main");
        // And HEAD followed the rename rather than falling off.
        match g.head().unwrap() {
            HeadState::Branch { name, .. } => assert_eq!(name.as_bytes(), b"trunk"),
            other => panic!("attached HEAD expected, got {other:?}"),
        }

        // Renaming onto an existing name is git's error, not a silent clobber.
        g.create_branch(b"side", None).unwrap();
        let e = g.rename_branch(b"side", b"trunk").unwrap_err();
        assert!(e.contains("already exists"), "{e}");
    }

    #[test]
    fn a_rename_keeps_a_non_utf8_name_byte_exact() {
        // The verb passes bytes to argv and reads bytes back from
        // for-each-ref; nothing between them may decode. Whether the volume
        // allows such a ref at all decides how far this proof runs: loose
        // refs are files named after their branch, and APFS refuses names
        // that are not UTF-8 outright.
        use std::os::unix::ffi::OsStrExt;
        let r = two_branches("branch-bytes");
        let latin1: std::ffi::OsString = std::ffi::OsStr::from_bytes(b"f\xe9ature").to_owned();
        if !r.git_os_try(&["branch".into(), latin1]) {
            return; // This volume refused; the plumbing stays proven by the
                    // ASCII rename above and the byte pass-through by the
                    // verb-level tests in `gitten-app` and the shell.
        }
        let g = r.open();
        g.rename_branch(b"f\xe9ature", b"ok").unwrap();
        let names: Vec<Vec<u8>> = g
            .branches()
            .unwrap()
            .iter()
            .map(|b| b.name.as_bytes().to_vec())
            .collect();
        assert!(!names.contains(&b"f\xe9ature".to_vec()));
        assert!(names.contains(&b"ok".to_vec()), "{names:?}");
    }

    #[test]
    fn a_plumbing_created_dash_ref_is_refused_instead_of_read_as_a_flag() {
        // Porcelain refuses to create this name; plumbing does not. Once it
        // exists, the bare-argv form of every verb would read it as an
        // *option* — `git checkout -q --detach` detaches HEAD, verified — so
        // the refusal has to be ours, before any process runs.
        let r = two_branches("branch-dash");
        r.git_os(&[
            "update-ref".into(),
            std::ffi::OsString::from("refs/heads/--weird"),
            std::ffi::OsString::from(r.rev_parse("HEAD")),
        ]);
        let g = r.open();
        let before = g.head().unwrap();

        // The verb that would have detached: refused in words that name
        // the rule, and HEAD exactly where it was.
        let e = g.checkout(b"--detach").unwrap_err();
        assert!(e.contains("'-'"), "{e}");
        assert_eq!(
            g.head().unwrap(),
            before,
            "a refused checkout moved nothing"
        );

        // Every verb that aims a name refuses the same way — including the
        // from side of a rename, which rides argv just as bare.
        for attempted in [
            g.create_branch(b"--x", None).err().unwrap(),
            g.delete_branch(b"--weird", false).err().unwrap(),
            g.rename_branch(b"--weird", b"ok").err().unwrap(),
            g.rename_branch(b"main", b"--x").err().unwrap(),
            g.create_branch(b"x", Some(b"--detach")).err().unwrap(),
        ] {
            assert!(attempted.contains("'-'"), "{attempted}");
        }

        // And showing is not aiming: the ref still lists, spelled as git
        // holds it, for the panel to render honestly.
        let names: Vec<Vec<u8>> = g
            .branches()
            .unwrap()
            .iter()
            .map(|b| b.name.as_bytes().to_vec())
            .collect();
        assert!(names.contains(&b"--weird".to_vec()), "{names:?}");
    }

    #[test]
    fn an_unborn_repository_answers_honestly_instead_of_inventing_state() {
        // Every fresh `git init`: HEAD names a branch no commit backs. The
        // reads say so (see [`HeadState::Branch`]'s `None`), and the verbs
        // pass git's own refusals through rather than pretending anything
        // moved.
        let r = Scratch::new("branch-unborn");
        let g = r.open();

        match g.head().unwrap() {
            HeadState::Branch { name, commit } => {
                assert_eq!(name.as_bytes(), b"main");
                assert_eq!(commit, None, "no commit exists to point at");
            }
            other => panic!("an unborn branch expected, got {other:?}"),
        }
        assert!(g.branches().unwrap().is_empty());

        // Checkout has nothing to move to, and says so in git's words.
        let e = g.checkout(b"main").unwrap_err();
        assert!(!e.trim().is_empty(), "{e}");
        // Creating at an implicit HEAD has nothing to resolve, same answer.
        let e = g.create_branch(b"fresh", None).unwrap_err();
        assert!(!e.trim().is_empty(), "{e}");
        // And still unborn — none of it half-happened.
        assert_eq!(
            g.head().unwrap(),
            HeadState::Branch {
                name: RefName::from("main"),
                commit: None,
            }
        );
    }

    #[test]
    fn pushing_nothing_refuses_instead_of_pointing_at_an_old_entry() {
        // The choice: an honest error, not Ok(0). Git exits zero here, but
        // there IS no new stash — and `Ok(0)` would name whatever already sat
        // at the top of the stack, a success badge over nothing happening.
        // The tree is dirty below so the first push proves the round-trip;
        // the second push, onto a clean tree, is the refusal under test.
        let r = Scratch::new("stash-noop");
        r.write("f.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        r.write("f.txt", b"two\n");
        let g = r.open();
        g.stash_push(None).expect("first push parks");
        let stack = g.stashes().unwrap();

        let e = g.stash_push(None).unwrap_err();
        assert!(e.contains("nothing to stash"), "{e}");
        assert_eq!(g.stashes().unwrap(), stack, "the stack did not move");
    }

    #[test]
    fn pushing_from_a_clean_tree_with_an_empty_stack_refuses_too() {
        // The third shape of "nothing": both rev-parses answer None — there
        // was no stack before and none after. `None == None` must read as
        // refusal, never as Ok(0) naming a top that does not exist.
        let r = Scratch::new("stash-noop-empty");
        r.write("f.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        let g = r.open();

        assert!(g.stashes().unwrap().is_empty());
        let e = g.stash_push(None).unwrap_err();
        assert!(e.contains("nothing to stash"), "{e}");
        assert!(
            g.stashes().unwrap().is_empty(),
            "still nothing on the stack"
        );
    }

    #[test]
    fn applying_restores_the_work_and_keeps_the_entry() {
        let r = with_dirty_tree("stash-apply");
        let g = r.open();
        g.stash_push(Some("keep me")).expect("pushes");

        g.stash_apply(0).expect("applies");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            b"one\ntwo\n"
        );
        assert_eq!(g.stashes().unwrap().len(), 1, "apply does not spend");
    }

    #[test]
    fn an_apply_that_would_stomp_local_changes_refuses_in_gits_words() {
        let r = with_dirty_tree("stash-conflict");
        let g = r.open();
        g.stash_push(None).expect("pushes");
        // A different edit to the same file: restoring the stash over it
        // would lose it, and git is the one that knows that.
        r.write("f.txt", b"one\nthree\n");

        let e = g.stash_apply(0).unwrap_err();
        assert!(e.contains("would be overwritten by merge"), "{e}");
    }

    #[test]
    fn a_pop_whose_apply_fails_leaves_the_stash_on_the_stack() {
        // The sequencing pop exists for is git's own: apply, then drop only
        // if the apply was clean. On conflict it keeps the entry and says so,
        // and this test is what stops this crate from ever trying to drop
        // separately — which is how a stash gets lost twice over.
        let r = with_dirty_tree("stash-pop-conflict");
        let g = r.open();
        g.stash_push(None).expect("pushes");
        r.write("f.txt", b"one\nthree\n");

        assert!(g.stash_pop(0).is_err());
        assert_eq!(g.stashes().unwrap().len(), 1, "kept, as git promised");
    }

    #[test]
    fn dropping_renumbers_from_the_top_so_an_index_never_lies_twice() {
        // Three pushes, two drops of index 0: what the second drop takes is
        // the former stash@{1}, because each verb re-derives its refname from
        // the index it is handed and git renumbers underneath. Holding the
        // old numbers across a write is exactly the bug this pins shut.
        let r = Scratch::new("stash-drop-renumber");
        r.write("seed.txt", b"seed\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "init"]);
        for (n, text) in ["a", "b", "c"].iter().enumerate() {
            r.write("seed.txt", format!("{text}\n").as_bytes());
            r.git(&["stash", "push", "-q", "-m", &format!("number {n}")]);
        }
        let g = r.open();
        assert_eq!(g.stashes().unwrap().len(), 3);
        let former_two = g.stashes().unwrap()[2].commit.clone();

        g.stash_drop(0).expect("drops the newest");
        let stack = g.stashes().unwrap();
        assert_eq!(stack.len(), 2);
        assert_eq!(
            stack[0].commit,
            r.rev_parse("stash@{0}"),
            "the read agrees with git about who is on top now"
        );
        g.stash_drop(0).expect("drops again");
        let stack = g.stashes().unwrap();
        assert_eq!(stack.len(), 1, "the former stash@{{1}} went first");
        assert_eq!(
            stack[0].index, 0,
            "indices are positions, recomputed by the read"
        );
        assert_eq!(
            stack[0].message, "On main: number 0",
            "the oldest survived both drops, under git's own prefix"
        );
        assert_eq!(
            stack[0].commit, former_two,
            "stash@{{0}} IS the former stash@{{1}}"
        );

        // Dropping the last one empties the stack; addressing past its end is
        // git's error, surfaced rather than translated.
        g.stash_drop(0).expect("drops the last");
        assert!(g.stashes().unwrap().is_empty());
        let e = g.stash_drop(0).unwrap_err();
        assert!(e.contains("git stash drop"), "{e}");
    }

    // ----------------------------------------------------- the history verbs

    /// Two commits where the second rewrote `f.txt`, so HEAD~1 has content
    /// of its own to go back to.
    fn two_commits(name: &str) -> Scratch {
        let r = Scratch::new(name);
        r.write("f.txt", b"first\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "first"]);
        r.write("f.txt", b"second\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "second"]);
        r
    }

    #[test]
    fn the_three_reset_strengths_leave_different_parts_behind() {
        // Soft: the branch alone moves, and the abandoned commit's change is
        // sitting in the index as if just staged.
        let r = two_commits("reset-soft");
        r.write("f.txt", b"second, edited\n");
        r.git(&["add", "-A"]);
        let g = r.open();
        let first = r.rev_parse("HEAD~1");
        g.reset(ResetMode::Soft, b"HEAD~1").expect("resets");
        assert_eq!(r.rev_parse("HEAD"), first, "branch moved");
        let tree = g.status().unwrap();
        assert_eq!(tree.staged.len(), 1, "the step back is staged");
        assert_eq!(tree.unstaged.len(), 0);
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            b"second, edited\n",
            "the working tree never moved"
        );

        // Mixed: the index goes too, so the same change comes back unstaged.
        let r = two_commits("reset-mixed");
        let g = r.open();
        let second = r.rev_parse("HEAD");
        g.reset(ResetMode::Mixed, b"HEAD~1").expect("resets");
        assert_ne!(r.rev_parse("HEAD"), second);
        let tree = g.status().unwrap();
        assert_eq!(tree.staged.len(), 0, "nothing stayed staged");
        assert_eq!(tree.unstaged.len(), 1, "and nothing was lost either");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            b"second\n",
            "the working tree keeps its files"
        );
        // Every abandoned commit stays reachable through the reflog, which is
        // what makes soft and mixed recoverable and hard the odd one out.
        // The reflog abbreviates, so the abandoned sha is matched by prefix.
        assert!(
            g.reflog(5)
                .unwrap()
                .iter()
                .any(|e| second.starts_with(e.commit.as_str())),
            "the abandoned commit survived in the reflog"
        );

        // Hard: all three move, and the changes are gone from everywhere the
        // status reads.
        let r = two_commits("reset-hard");
        r.write("f.txt", b"unsaved work\n");
        let g = r.open();
        assert_eq!(g.log(5).unwrap().len(), 2);
        g.reset(ResetMode::Hard, b"HEAD~1").expect("resets");
        assert_eq!(g.log(5).unwrap().len(), 1, "history shortened");
        let tree = g.status().unwrap();
        assert_eq!(tree.staged.len() + tree.unstaged.len(), 0, "all quiet");
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            b"first\n",
            "the file went back to what the target says"
        );
    }

    #[test]
    fn a_reset_aimed_at_a_bare_oid_moves_whatever_head_points_at() {
        // Named by its own object id, not a HEAD-relative spelling — the
        // shape a commits row carries. Detached, so the verb must move HEAD
        // itself rather than a branch under it.
        let r = two_commits("reset-detached");
        r.git(&["checkout", "-q", "--detach", "HEAD"]);
        let g = r.open();
        let target = r.rev_parse("HEAD~1");

        g.reset(ResetMode::Hard, target.as_bytes()).expect("resets");
        assert_eq!(r.rev_parse("HEAD"), target);
        match g.head().unwrap() {
            HeadState::Detached { commit } => assert_eq!(commit, target),
            other => panic!("detached expected, got {other:?}"),
        }
    }

    #[test]
    fn reset_and_revert_targets_beginning_with_a_dash_are_refused_before_git() {
        let r = two_commits("history-dash");
        let before = r.rev_parse("HEAD");
        let g = r.open();
        // A revspec spelled like an option would arrive in argv AS an option;
        // the guard answers in words instead of letting git guess.
        for mode in [ResetMode::Soft, ResetMode::Mixed, ResetMode::Hard] {
            let e = g.reset(mode, b"--hard").unwrap_err();
            assert!(e.contains("refused"), "{mode:?}: {e}");
        }
        let e = g.revert(b"-m 1").unwrap_err();
        assert!(e.contains("refused"), "{e}");
        assert_eq!(r.rev_parse("HEAD"), before, "nothing ran");
    }

    #[test]
    fn a_revert_lands_an_inverse_commit_and_leaves_the_tree_matching_the_parent() {
        let r = two_commits("revert-clean");
        let g = r.open();

        g.revert(b"HEAD").expect("reverts");
        assert_eq!(g.log(5).unwrap().len(), 3, "a third commit appeared");
        // The undo's tree is what history held two steps back — the state
        // before the undone commit existed.
        assert_eq!(r.rev_parse("HEAD^{tree}"), r.rev_parse("HEAD~2^{tree}"));
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            b"first\n",
            "what the reverted commit did, the undo undid"
        );
        let tree = g.status().unwrap();
        assert_eq!(
            tree.staged.len() + tree.unstaged.len(),
            0,
            "the revert committed itself"
        );
    }

    #[test]
    fn a_reverting_conflict_refuses_in_gits_own_words_and_changes_no_history() {
        let r = Scratch::new("revert-conflict");
        r.write("f.txt", b"one\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "one"]);
        r.write("f.txt", b"one\ntwo\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "two"]);
        r.write("f.txt", b"ONE\nTWO\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "three"]);
        let g = r.open();

        // Undoing the middle commit means removing "two", which later work
        // rewrote into "TWO"; git stops and asks, and that question is the
        // answer this verb gives — verbatim, its own diagnosis included.
        let e = g.revert(r.rev_parse("HEAD~1").as_bytes()).unwrap_err();
        assert!(e.contains("could not revert"), "{e}");
        assert!(!g.status().unwrap().conflicts.is_empty(), "left conflicted");
        assert_eq!(g.log(5).unwrap().len(), 3, "nothing landed");
    }

    #[test]
    fn an_amend_replaces_head_with_new_message_and_staged_content() {
        let r = two_commits("amend-roundtrip");
        let old = r.rev_parse("HEAD");
        let parent = r.rev_parse("HEAD~1");
        r.write("f.txt", b"second, amended\n");
        r.git(&["add", "-A"]);

        let message = "second, rewritten\n\nwith a body & \"quotes\"\n";
        let sha = r.open().amend(message).expect("amends");
        assert_eq!(sha, r.rev_parse("HEAD"));
        assert_ne!(sha, old, "the commit was replaced, not kept");
        assert_eq!(r.rev_parse("HEAD~1"), parent, "the parent stood still");

        // The stored bytes, read back through git itself — %B is the whole
        // message, body included. git appends the one final newline it
        // promises every commit; everything else is verbatim.
        let raw = String::from_utf8(r.git_os_out(&[
            "show".into(),
            "-s".into(),
            "--format=%B".into(),
            sha.into(),
        ]))
        .unwrap();
        assert_eq!(raw.trim_end_matches('\n'), message.trim_end_matches('\n'));
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"f.txt")).unwrap(),
            b"second, amended\n",
            "the index rode along"
        );
    }

    #[test]
    fn an_unborn_or_empty_amend_is_refused_before_any_process_runs() {
        let r = Scratch::new("amend-unborn");
        r.write("f.txt", b"x\n");
        let g = r.open();
        match g.head().unwrap() {
            HeadState::Branch { commit: None, .. } => {}
            other => panic!("unborn expected, got {other:?}"),
        }
        let e = g.amend("too soon").unwrap_err();
        assert!(e.contains("no commits yet"), "{e}");
        assert!(g.log(1).is_err() || g.log(1).unwrap().is_empty());

        // Same refusal an empty commit gets: said here, not discovered in a
        // hook's output afterwards.
        let r = two_commits("amend-empty");
        let sha = r.rev_parse("HEAD");
        for empty in ["", "  \n\t"] {
            let e = r.open().amend(empty).unwrap_err();
            assert!(e.contains("message"), "{empty:?}: {e}");
        }
        assert_eq!(r.rev_parse("HEAD"), sha, "the refusals left HEAD alone");
    }

    // ------------------------------------------------------ the sync verbs

    /// The tracking pair the branches read reports for `main`, as counts.
    fn main_counts(r: &Scratch) -> (Option<u32>, Option<u32>) {
        branch(&r.open().branches().unwrap(), "main")
            .upstream
            .as_ref()
            .map(|u| (u.ahead, u.behind))
            .expect("main tracks origin")
    }

    #[test]
    fn a_first_push_sets_the_upstream_and_a_second_leaves_it_alone() {
        let origin = Scratch::bare("sync-upstream-origin");
        let r = Scratch::new("sync-upstream");
        r.write("seed.txt", b"seed\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "seed"]);
        r.git(&[
            "remote",
            "add",
            "origin",
            &format!("{}", origin.0.display()),
        ]);

        let g = r.open();
        g.push(b"origin", b"main").expect("the first push");

        // The remote holds what was sent, and the branch now tracks it —
        // measured through the same branches read that made the decision.
        assert_eq!(origin.rev_parse("refs/heads/main"), r.rev_parse("main"));
        let branches = g.branches().unwrap();
        let up = branch(&branches, "main")
            .upstream
            .as_ref()
            .expect("the first push set it");
        assert_eq!(up.remote.as_bytes(), b"origin");
        assert_eq!(up.branch.as_bytes(), b"main");
        assert_eq!((up.ahead, up.behind), (Some(0), Some(0)));

        // And a second push does not touch the configuration it found: the
        // tracking pair is still exactly the pair the first push wrote.
        let config = ["config".into(), "--get-regexp".into(), "^branch\\.".into()];
        let before = r.git_os_out(&config);
        assert!(!before.is_empty(), "the config exists to be left alone");
        g.push(b"origin", b"main").expect("the second push");
        assert_eq!(
            r.git_os_out(&config),
            before,
            "an existing upstream is none of a push's business"
        );
    }

    #[test]
    fn a_fast_forward_pull_moves_the_branch_and_the_tree() {
        let (r, origin) = upstream_fixture("sync-ff");
        // The other machine moves; this side has not.
        let twin = Scratch::cloned(&origin.0, "sync-ff-twin");
        twin.write("t.txt", b"theirs\n");
        twin.git(&["add", "-A"]);
        twin.git(&["commit", "-qm", "theirs"]);
        twin.git(&["push", "-q", "origin", "main"]);

        r.open().pull().expect("a clean fast-forward");

        assert_eq!(r.rev_parse("HEAD"), twin.rev_parse("HEAD"));
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"t.txt")).unwrap(),
            b"theirs\n",
            "the working tree came along, as a pull owes"
        );
    }

    #[test]
    fn a_diverged_pull_refuses_verbatim_and_touches_nothing() {
        let (r, origin) = upstream_fixture("sync-diverged");
        // Ours: a commit only this side has.
        r.write("ours.txt", b"ours\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "ours"]);
        // Theirs: a commit only the remote has, already fetched.
        let twin = Scratch::cloned(&origin.0, "sync-diverged-twin");
        twin.write("theirs.txt", b"theirs\n");
        twin.git(&["add", "-A"]);
        twin.git(&["commit", "-qm", "theirs"]);
        twin.git(&["push", "-q", "origin", "main"]);
        r.git(&["fetch", "-q", "origin"]);

        let ours = r.rev_parse("HEAD");
        let err = r.open().pull().unwrap_err();
        assert!(
            err.to_lowercase().contains("fast-forward"),
            "git's own refusal, verbatim: {err}"
        );

        // Refused means refused: HEAD stays on our commit, their commit is
        // nowhere in our history, and both working-tree files are exactly
        // as the tree held them. No auto-rebase tidies this up behind the
        // reader's back.
        assert_eq!(r.rev_parse("HEAD"), ours);
        assert!(!join_raw(&r.0, b"theirs.txt").exists());
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"ours.txt")).unwrap(),
            b"ours\n"
        );
    }

    #[test]
    fn pulling_without_an_upstream_refuses_in_gits_own_words() {
        let origin = Scratch::bare("sync-noupstream-origin");
        let r = Scratch::new("sync-noupstream");
        r.write("seed.txt", b"seed\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "seed"]);
        // A remote, but no `-u`: nothing says where main pulls from.
        r.git(&[
            "remote",
            "add",
            "origin",
            &format!("{}", origin.0.display()),
        ]);

        let seed = r.rev_parse("HEAD");
        let err = r.open().pull().unwrap_err();
        assert!(
            err.to_lowercase().contains("tracking information"),
            "git's own sentence names the missing configuration: {err}"
        );
        assert_eq!(r.rev_parse("HEAD"), seed, "refused, so unmoved");
    }

    #[test]
    fn a_fetch_updates_exactly_the_remote_tracking_refs() {
        let (r, origin) = upstream_fixture("sync-fetch");
        let twin = Scratch::cloned(&origin.0, "sync-fetch-twin");
        twin.write("t.txt", b"t\n");
        twin.git(&["add", "-A"]);
        twin.git(&["commit", "-qm", "theirs"]);
        twin.git(&["push", "-q", "origin", "main"]);

        // Before the fetch the local view cannot know: in sync at 0/0,
        // because the tracking ref still names yesterday's commit.
        assert_eq!(main_counts(&r), (Some(0), Some(0)));
        r.open().fetch(Some(b"origin")).expect("the fetch");

        // After: the ref moved and the count arrived with it — behind is
        // the fetch's whole story, told without touching anything else.
        assert_eq!(
            r.rev_parse("refs/remotes/origin/main"),
            twin.rev_parse("HEAD")
        );
        assert_eq!(main_counts(&r), (Some(0), Some(1)));
        assert_eq!(
            std::fs::read(join_raw(&r.0, b"seed.txt")).unwrap(),
            b"seed\n",
            "a fetch never touches the working tree"
        );
    }

    #[test]
    fn a_nameless_fetch_takes_every_remote_at_once() {
        let a = Scratch::bare("sync-all-a");
        let b = Scratch::bare("sync-all-b");
        let r = Scratch::new("sync-all");
        r.write("seed.txt", b"seed\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "seed"]);
        for (name, remote) in [("a", &a), ("b", &b)] {
            r.git(&["remote", "add", name, &format!("{}", remote.0.display())]);
            r.git(&["push", "-q", name, "main"]);
        }
        // Each remote gains a branch of its own, pushed by its own twin, and
        // the twin's commit is what the local tracking ref must come to name.
        let mut theirs = Vec::new();
        for (name, remote) in [("a", &a), ("b", &b)] {
            let twin = Scratch::cloned(&remote.0, &format!("sync-all-{name}-twin"));
            twin.write("x.txt", b"x\n");
            twin.git(&["add", "-A"]);
            twin.git(&["commit", "-qm", "ahead"]);
            twin.git(&["push", "-q", "origin", &format!("main:side-{name}")]);
            theirs.push((
                format!("refs/remotes/{name}/side-{name}"),
                twin.rev_parse("HEAD"),
            ));
        }

        r.open().fetch(None).expect("fetches everything");

        // Named on neither command line: both tracking refs moved anyway,
        // which is what `--all` was spelled for.
        for (refname, oid) in &theirs {
            assert_eq!(&r.rev_parse(refname), oid, "{refname} arrived");
        }
    }

    #[test]
    fn ahead_and_behind_move_through_fetch_pull_commit_and_push_end_to_end() {
        let (r, origin) = upstream_fixture("sync-counts");
        let twin = Scratch::cloned(&origin.0, "sync-counts-twin");
        twin.write("t.txt", b"t\n");
        twin.git(&["add", "-A"]);
        twin.git(&["commit", "-qm", "theirs"]);
        twin.git(&["push", "-q", "origin", "main"]);
        let g = r.open();

        // Fetch: the remote's move becomes visible as behind.
        g.fetch(Some(b"origin")).expect("fetch");
        assert_eq!(main_counts(&r), (Some(0), Some(1)), "behind, seen");

        // Pull: the distance closes to nothing, on both sides of the pair.
        g.pull().expect("fast-forward");
        assert_eq!(main_counts(&r), (Some(0), Some(0)));

        // A local commit opens the other direction: what a push would send.
        r.write("mine.txt", b"mine\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "mine"]);
        assert_eq!(main_counts(&r), (Some(1), Some(0)), "ahead, earned");

        // Push closes it — and does not reopen behind, because sending is
        // all a push does.
        g.push(b"origin", b"main").expect("push");
        assert_eq!(main_counts(&r), (Some(0), Some(0)));
        assert_eq!(origin.rev_parse("refs/heads/main"), r.rev_parse("main"));
    }

    #[test]
    fn sync_names_beginning_with_a_dash_are_refused_before_git_sees_them() {
        let (r, _) = upstream_fixture("sync-dash");
        let g = r.open();
        // `--upload-pack` runs a program of its choosing; a remote spelled
        // to look like one is precisely why the guard stands ahead of argv.
        let e = g.fetch(Some(b"--upload-pack=touch /tmp/x")).unwrap_err();
        assert!(e.contains("refused"), "{e}");
        let e = g.push(b"-oProxyCommand=x", b"main").unwrap_err();
        assert!(e.contains("refused"), "{e}");
        let e = g.push(b"origin", b"-b").unwrap_err();
        assert!(e.contains("refused"), "{e}");

        // And nothing ran: HEAD never moved, no process answered any of it.
        assert_eq!(r.rev_parse("HEAD"), r.rev_parse("main"));
    }

    // ------------------------------------------------------------- the rebase

    use gitten_core::rebase::{Action, Line, Rewrite, TodoScript};

    /// A straight line of work over separate files: `base`, then three
    /// commits each adding its own file, so a rewrite that loses content
    /// shows in the tree and not only in the log.
    fn linear_repo(name: &str) -> Scratch {
        let s = Scratch::new(name);
        s.write("base.txt", b"base\n");
        s.git(&["add", "-A"]);
        s.git(&["commit", "-qm", "base"]);
        for (file, body, msg) in [
            ("one.txt", &b"one\n"[..], "one"),
            ("two.txt", &b"two\n"[..], "two"),
            ("three.txt", &b"three\n"[..], "three"),
        ] {
            s.write(file, body);
            s.git(&["add", "-A"]);
            s.git(&["commit", "-qm", msg]);
        }
        s
    }

    fn subjects(r: &Scratch) -> Vec<String> {
        let out = r.git_os_out(&["log".into(), "--format=%s".into(), "--topo-order".into()]);
        String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The full message of the commit a rev names — squash's concat rules
    /// live here and nowhere else.
    fn body_of(r: &Scratch, rev: &str) -> String {
        let out = r.git_os_out(&["log".into(), "-1".into(), "--format=%B".into(), rev.into()]);
        String::from_utf8_lossy(&out).into_owned()
    }

    /// A plan of bare picks over full shas, oldest first — what most of
    /// these tests need and nothing more.
    fn picks(revs: &[&str]) -> TodoScript {
        let mut script = TodoScript::default();
        for rev in revs {
            script.push_step(Action::Pick, rev.as_bytes());
        }
        script
    }

    #[test]
    fn a_reordered_plan_moves_the_commits_and_keeps_the_tree() {
        let r = linear_repo("rebase-reorder");
        let base = r.rev_parse("HEAD~3");
        let (one, two, three) = (
            r.rev_parse("HEAD~2"),
            r.rev_parse("HEAD~1"),
            r.rev_parse("HEAD"),
        );
        let g = r.open();
        assert!(!g.rebase_in_progress());

        // Two and one swap places: the plan reads oldest first, so [two, one,
        // three] builds base←two←one←three and the log reads back three, one,
        // two over base.
        g.rebase_todo(base.as_bytes(), &picks(&[&two, &one, &three]))
            .expect("the reorder ran");
        assert_eq!(
            subjects(&r),
            vec!["three", "one", "two", "base"],
            "history reads in the plan's order"
        );
        // Every file survived: a reorder moves commits, never their changes,
        // and a clean finish leaves no state behind it.
        let tree = g.status().expect("status");
        assert!(tree.staged.is_empty() && tree.unstaged.is_empty());
        for name in ["base.txt", "one.txt", "two.txt", "three.txt"] {
            assert!(std::path::Path::new(&r.0).join(name).exists(), "{name}");
        }
        assert!(!g.rebase_in_progress());
    }

    #[test]
    fn a_dropped_pick_leaves_its_change_out_of_the_tree() {
        let r = linear_repo("rebase-drop");
        let base = r.rev_parse("HEAD~3");
        let (one, three) = (r.rev_parse("HEAD~2"), r.rev_parse("HEAD"));
        let g = r.open();

        g.rebase_todo(base.as_bytes(), &picks(&[&one, &three]))
            .expect("the drop ran");
        assert_eq!(subjects(&r), vec!["three", "one", "base"], "two is gone");
        assert!(
            !std::path::Path::new(&r.0).join("two.txt").exists(),
            "and its change left the branch with it"
        );
    }

    #[test]
    fn squash_melds_messages_by_gits_own_rule_and_fixup_discards_them() {
        // Squash opens GIT_EDITOR on a template that already holds both
        // messages, separated by comment lines git strips afterwards — so
        // with the editor answered `true`, the melded message is exactly
        // first + blank line + second, git's own rule and not ours. Fixup
        // never opens an editor at all.
        let r = linear_repo("rebase-squash");
        let base = r.rev_parse("HEAD~3");
        let (one, two, three) = (
            r.rev_parse("HEAD~2"),
            r.rev_parse("HEAD~1"),
            r.rev_parse("HEAD"),
        );
        let mut squashed = picks(&[&one]);
        squashed.push_step(Action::Squash, two.as_bytes());
        squashed.push_step(Action::Pick, three.as_bytes());
        r.open()
            .rebase_todo(base.as_bytes(), &squashed)
            .expect("squash ran");
        assert_eq!(
            subjects(&r),
            vec!["three", "one", "base"],
            "three commits became two"
        );
        assert_eq!(
            body_of(&r, "HEAD~1").trim_end(),
            "one\n\ntwo",
            "both messages survive, blank line between"
        );

        // The fixup shape, on a fresh straight line.
        let r2 = linear_repo("rebase-fixup");
        let base2 = r2.rev_parse("HEAD~3");
        let (one2, two2, three2) = (
            r2.rev_parse("HEAD~2"),
            r2.rev_parse("HEAD~1"),
            r2.rev_parse("HEAD"),
        );
        let mut fixed = picks(&[&one2]);
        fixed.push_step(Action::Fixup, two2.as_bytes());
        fixed.push_step(Action::Pick, three2.as_bytes());
        let g2 = r2.open();
        g2.rebase_todo(base2.as_bytes(), &fixed).expect("fixup ran");
        assert_eq!(
            body_of(&r2, "HEAD~1").trim_end(),
            "one",
            "the melded commit keeps the first message alone"
        );
        assert!(
            std::path::Path::new(&r2.0).join("one.txt").exists(),
            "but its change stayed"
        );
    }

    #[test]
    fn a_composed_squash_of_the_second_visible_commit_runs_to_completion() {
        // The shell-level path end to end: log the repository the way a
        // pane does, hand that window to `core::rebase::compose`, aim the
        // plan at git through the trait verb. This is exactly the shape
        // that once opened its plan with a bare squash — which git refuses,
        // stranding `.git/rebase-merge` behind exit 1 — so it is proven
        // here against real git and not only against the model.
        let r = linear_repo("rebase-compose-squash");
        let g = r.open();
        let history = g.log(500).expect("log");
        let index = history
            .iter()
            .position(|c| c.subject == "two")
            .expect("two sits in the window");
        let before = history.len();

        let (upstream, script) =
            gitten_core::rebase::compose(Rewrite::SquashUp, &history, index).expect("composes");
        g.rebase_todo(&upstream, &script)
            .expect("the composed squash ran");

        assert!(!g.rebase_in_progress(), "git completed the whole plan");
        assert_eq!(
            g.log(500).expect("log").len(),
            before - 1,
            "two commits became one"
        );
        assert_eq!(
            body_of(&r, "HEAD~1").trim_end(),
            "one\n\ntwo",
            "melded by git's own concat rule, blank line between"
        );
        let tree = g.status().expect("status");
        assert!(tree.staged.is_empty() && tree.unstaged.is_empty());
    }

    #[test]
    fn an_exec_line_runs_between_the_picks_without_becoming_a_commit() {
        let r = linear_repo("rebase-exec");
        let base = r.rev_parse("HEAD~3");
        let (one, three) = (r.rev_parse("HEAD~2"), r.rev_parse("HEAD"));
        let mut script = picks(&[&three]);
        script.push_step(Action::Exec, b"echo executed > executed.txt");
        script.push_step(Action::Pick, one.as_bytes());

        r.open()
            .rebase_todo(base.as_bytes(), &script)
            .expect("exec ran");
        assert_eq!(
            subjects(&r),
            vec!["one", "three", "base"],
            "three commits went in, three came out"
        );
    }

    #[test]
    fn reword_and_edit_are_refused_before_any_process_runs() {
        let r = linear_repo("rebase-reword");
        let before = r.rev_parse("HEAD");
        let g = r.open();

        let mut bad = TodoScript::default();
        bad.push_step(Action::Pick, b"aabbccd");
        bad.push_step(Action::Reword, b"ddeeff0");
        let err = g.rebase_todo(b"HEAD~3", &bad).unwrap_err();
        assert!(err.contains("reword"), "{err}");

        // Nothing started: no state directory, HEAD where it was.
        assert!(!g.rebase_in_progress(), "the refusal predated any process");
        assert_eq!(r.rev_parse("HEAD"), before);
    }

    #[test]
    fn a_conflicted_rebase_is_found_by_progress_and_undone_by_abort() {
        // Two commits editing the same line; replaying them swapped makes
        // the second pick conflict — exactly the mid-flight state the abort
        // story exists for.
        let r = Scratch::new("rebase-conflict");
        r.write("line.txt", b"start\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "base"]);
        r.write("line.txt", b"first\n");
        r.git(&["commit", "-qam", "first"]);
        r.write("line.txt", b"second\n");
        r.git(&["commit", "-qam", "second"]);

        let g = r.open();
        let original_head = r.rev_parse("HEAD");
        let base = r.rev_parse("HEAD~2");

        let err = g
            .rebase_todo(
                base.as_bytes(),
                &picks(&[&original_head, &r.rev_parse("HEAD~1")]),
            )
            .unwrap_err();
        assert!(!err.is_empty(), "git's own words come back");
        assert!(
            g.rebase_in_progress(),
            "the failed rebase left its state to be found"
        );

        g.rebase_abort().expect("abort");
        assert!(!g.rebase_in_progress(), "abort cleaned up after itself");
        assert_eq!(r.rev_parse("HEAD"), original_head, "back where it began");
        let tree = g.status().expect("status");
        assert!(
            tree.staged.is_empty() && tree.unstaged.is_empty(),
            "and the working tree came home too"
        );
    }

    #[test]
    fn continue_finishes_what_a_human_resolved() {
        // Two commits editing the same line, replayed swapped: every pick
        // conflicts, and each is resolved by hand through the scratch
        // harness (`--theirs`, the side being replayed), then driven onward
        // through the verb — both editors answered with `true`, so nothing
        // blocks on a prompt nobody can see.
        let r = Scratch::new("rebase-continue");
        r.write("line.txt", b"start\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "base"]);
        r.write("line.txt", b"first\n");
        r.git(&["commit", "-qam", "first"]);
        r.write("line.txt", b"second\n");
        r.git(&["commit", "-qam", "second"]);

        let g = r.open();
        let base = r.rev_parse("HEAD~2");
        let _ = g.rebase_todo(
            base.as_bytes(),
            &picks(&[&r.rev_parse("HEAD"), &r.rev_parse("HEAD~1")]),
        );
        assert!(g.rebase_in_progress());

        for round in 1..=4 {
            if !g.rebase_in_progress() {
                break;
            }
            let conflicted = std::process::Command::new("git")
                .arg("-C")
                .arg(&r.0)
                .args(SCRATCH_CONFIG)
                .args(["diff", "--name-only", "--diff-filter=U"])
                .output()
                .expect("conflicted paths listed");
            assert!(
                !conflicted.stdout.is_empty(),
                "round {round}: nothing stood unresolved"
            );
            r.git(&["checkout", "--theirs", "line.txt"]);
            r.git(&["add", "line.txt"]);
            // Continuing past one pick can walk straight into the next
            // pick's conflict: git answers nonzero with its own words and
            // the state left standing — which is exactly what the verb
            // promises to surface, so the loop reads it rather than dies.
            let _ = g.rebase_continue();
        }
        assert!(!g.rebase_in_progress());
        assert_eq!(subjects(&r), vec!["first", "second", "base"]);
    }

    #[test]
    fn a_dirty_tree_is_gits_own_refusal_surfaced_verbatim() {
        let r = linear_repo("rebase-dirty");
        let base = r.rev_parse("HEAD~3");
        let head = r.rev_parse("HEAD");
        r.write("one.txt", b"changed, unstaged\n");
        let g = r.open();

        let err = g
            .rebase_todo(base.as_bytes(), &picks(&[&head]))
            .unwrap_err();
        assert!(err.contains("unstaged"), "{err}");
        assert!(!g.rebase_in_progress());

        let err = g.rebase_onto(b"HEAD~1").unwrap_err();
        assert!(err.contains("unstaged"), "{err}");
        assert_eq!(r.rev_parse("HEAD"), head, "nothing moved");
    }

    /// git's own todo file, saved by pointing `GIT_SEQUENCE_EDITOR` at a cp
    /// that saves instead of installs — the same mechanism
    /// [`Repo::rebase_todo`] drives, aimed the other way. Parsing what git
    /// actually wrote and emitting it back byte-exact is the golden test;
    /// feeding our emitted bytes through another rebase proves git accepts
    /// them as its plan.
    #[test]
    fn gits_own_todo_round_trips_through_the_model_and_git_accepts_it_back() {
        let r = linear_repo("rebase-golden");
        let g = r.open();
        let base = r.rev_parse("HEAD~3");

        let saved = std::env::temp_dir().join(format!("gitten-golden-{}", std::process::id()));
        let _ = std::fs::remove_file(&saved);
        // git appends the todo path to the editor command, so a plain cp
        // would copy the wrong way round for *saving*. A redirect does it:
        // `cat > <saved> "<todo>"` — git's shell wrapping supplies the todo
        // as one more argument and cat writes its content into the file.
        let saved_shown = saved.display();
        let editor = format!("cat > '{saved_shown}'");
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&r.0)
            .args(SCRATCH_CONFIG)
            .args(["rebase", "-i"])
            .arg(&base)
            .env("GIT_SEQUENCE_EDITOR", &editor)
            .output()
            .expect("rebase -i runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let raw = std::fs::read(&saved).expect("the editor saw the todo");
        let _ = std::fs::remove_file(&saved);

        // What git generated parses into three understood picks plus its
        // comment header — and emits byte-exact.
        let script = TodoScript::parse(&raw);
        assert_eq!(raw, script.emit(), "our emitter reproduces git's file");
        let steps: Vec<_> = script
            .lines()
            .iter()
            .filter_map(|l| match l {
                Line::Step(s) => Some(s.clone()),
                Line::Verbatim(_) => None,
            })
            .collect();
        assert_eq!(steps.len(), 3);
        assert!(steps.iter().all(|s| s.action == Action::Pick));
        assert!(
            script
                .lines()
                .iter()
                .any(|l| matches!(l, Line::Verbatim(raw) if raw.starts_with(b"#"))),
            "the header rides along"
        );

        // Swap the two oldest picks in the model and hand the result to git:
        // the identity replay above put three on top already, so the newest
        // three subjects should now read two, one, three.
        let mut reordered = TodoScript::default();
        let mut order = steps.clone();
        order.swap(0, 1);
        for step in order {
            reordered.push_step(step.action, &step.arg);
        }
        g.rebase_todo(base.as_bytes(), &reordered)
            .expect("git accepted our plan");
        assert_eq!(
            subjects(&r),
            vec!["three", "one", "two", "base"],
            "the swap in the plan is the swap in history"
        );
    }

    #[test]
    fn plain_rebase_onto_moves_a_branch_and_conflicts_surface_the_same_way() {
        // topic has one commit; main moves sideways; rebasing topic onto
        // main replays topic's commit on top of main's tip.
        let r = Scratch::new("rebase-onto");
        r.write("f.txt", b"base\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "base"]);
        r.git(&["checkout", "-qb", "topic"]);
        r.write("topic.txt", b"topic\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "topic-one"]);
        r.git(&["checkout", "-q", "main"]);
        r.write("main.txt", b"main\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "main-move"]);
        r.git(&["checkout", "-q", "topic"]);
        let main_tip = r.rev_parse("main");

        let g = r.open();
        g.rebase_onto(b"main").expect("rebase onto");
        assert_eq!(r.rev_parse("HEAD~1"), main_tip, "topic now sits on main");
        assert_eq!(subjects(&r), vec!["topic-one", "main-move", "base"]);
        assert!(!g.rebase_in_progress());

        // Conflicts leave the same findable state the interactive path does.
        r.git(&["checkout", "-qb", "clash", "main"]);
        r.write("shared.txt", b"from clash\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "clash-one"]);
        r.git(&["checkout", "-q", "main"]);
        r.write("shared.txt", b"from main\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "main-clash"]);
        r.git(&["checkout", "-q", "clash"]);
        let before = r.rev_parse("clash");
        assert!(g.rebase_onto(b"main").is_err());
        assert!(g.rebase_in_progress(), "found by the progress read");
        g.rebase_abort().expect("abort");
        assert_eq!(r.rev_parse("clash"), before, "aborted clean");
    }

    #[test]
    fn todo_tempfiles_are_private_unique_and_cleaned_up() {
        // The plan carries commit subjects and lands in a shared directory,
        // so the file answers to a stricter contract than convenience:
        // owner-only however the umask feels, never the same name twice,
        // bytes intact, gone when the caller removes it.
        let first = write_todo_tmpfile(b"pick 1111111\n".to_vec()).expect("first");
        let second = write_todo_tmpfile(b"pick 2222222\n".to_vec()).expect("second");
        assert_ne!(first, second, "two plans never share a file");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&first).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{:?} is not owner-only", first);
        assert_eq!(std::fs::read(&first).unwrap(), b"pick 1111111\n");
        std::fs::remove_file(&first).unwrap();
        assert!(!first.exists());
        std::fs::remove_file(&second).unwrap();
    }

    #[test]
    fn a_stranded_rebase_is_reached_by_abort_and_put_back() {
        // A plan whose first line is a squash — the shape compose() once
        // emitted, and exactly what git refuses with "cannot 'squash'
        // without a previous commit" — strands the rebase deliberately:
        // exit nonzero, state left standing. This is the state the
        // rebase.abort command exists to walk out of.
        let r = linear_repo("rebase-stranded");
        let g = r.open();
        let original = r.rev_parse("main");

        let mut stranded = TodoScript::default();
        stranded.push_step(Action::Squash, r.rev_parse("HEAD~1").as_bytes());
        assert!(g
            .rebase_todo(r.rev_parse("HEAD~3").as_bytes(), &stranded)
            .is_err());
        assert!(
            g.rebase_in_progress(),
            "git's refusal left its state to be found"
        );

        // The command's verb, driven directly: everything comes home.
        g.rebase_abort().expect("abort");
        assert!(!g.rebase_in_progress());
        assert_eq!(r.rev_parse("HEAD"), original, "back where it began");
        let tree = g.status().expect("status");
        assert!(tree.staged.is_empty() && tree.unstaged.is_empty());
    }

    #[test]
    fn an_upstream_spelled_like_a_flag_is_refused_before_any_process_runs() {
        let r = linear_repo("rebase-dashes");
        let g = r.open();
        let head = r.rev_parse("HEAD");
        let empty = TodoScript::default();
        let err = g.rebase_todo(b"--exec=touch /tmp/x", &empty).unwrap_err();
        assert!(err.contains("refused"), "{err}");
        let err = g.rebase_onto(b"--exec=touch /tmp/x").unwrap_err();
        assert!(err.contains("refused"), "{err}");
        assert_eq!(r.rev_parse("HEAD"), head, "nothing ran");
    }

    // ------------------------------------------------------- cherry-picking

    #[test]
    fn a_clean_cherry_pick_grows_the_branch_and_leaves_the_tree_clean() {
        // side carries one commit of its own; picking it onto main replays
        // that change as a new commit on main's tip. Nothing moves: side
        // still points where it did, and main has simply grown.
        let r = linear_repo("pick-clean");
        r.git(&["checkout", "-qb", "side", "main"]);
        r.write("side.txt", b"from side\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "side-one"]);
        r.git(&["checkout", "-q", "main"]);
        r.write("main.txt", b"from main\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "main-move"]);
        let main_tip = r.rev_parse("HEAD");
        let picked = r.rev_parse("side");
        let g = r.open();
        assert!(!g.cherry_pick_in_progress());

        g.cherry_pick(picked.as_bytes()).expect("the pick ran");
        assert_eq!(
            subjects(&r),
            vec!["side-one", "main-move", "three", "two", "one", "base"],
            "the copy landed on main's tip"
        );
        assert_eq!(
            r.rev_parse("HEAD~1"),
            main_tip,
            "the new commit sits exactly one above where main was"
        );
        // The change itself arrived, and the tree is clean afterwards.
        assert!(std::path::Path::new(&r.0).join("side.txt").exists());
        let tree = g.status().expect("status");
        assert!(tree.staged.is_empty() && tree.unstaged.is_empty());
        assert!(!g.cherry_pick_in_progress(), "nothing left standing");

        // And the source branch never moved.
        assert_eq!(r.rev_parse("side"), picked);
    }

    #[test]
    fn a_conflicted_pick_is_found_by_progress_refuses_a_second_and_abort_puts_it_back() {
        let r = two_branches_editing_one_file("pick-clash");
        let g = r.open();
        let before = r.rev_parse("main");

        assert!(g.cherry_pick(r.rev_parse("topic").as_bytes()).is_err());
        assert!(
            g.cherry_pick_in_progress(),
            "git's refusal left its state to be found"
        );
        // One sequencer: a second start cannot share the index with the
        // first, and the refusal says so rather than disturbing it.
        let err = g.cherry_pick(r.rev_parse("topic").as_bytes()).unwrap_err();
        assert!(err.contains("already in progress"), "{err}");

        // The conflict itself is in the index, where a status read finds it.
        let tree = g.status().expect("status");
        assert_eq!(tree.conflicts.len(), 1, "{tree:?}");
        assert_eq!(
            tree.conflicts[0].path.as_bytes(),
            b"shared.txt",
            "the unmerged path git stopped on"
        );

        g.cherry_pick_abort().expect("abort");
        assert!(!g.cherry_pick_in_progress());
        assert_eq!(r.rev_parse("HEAD"), before, "back where it began");
        let tree = g.status().expect("status");
        assert!(tree.staged.is_empty() && tree.unstaged.is_empty() && tree.conflicts.is_empty());
    }

    #[test]
    fn a_resolved_pick_continues_to_a_commit_of_its_own() {
        let r = two_branches_editing_one_file("pick-continue");
        let g = r.open();

        assert!(g.cherry_pick(r.rev_parse("topic").as_bytes()).is_err());
        assert!(g.cherry_pick_in_progress());

        // A human resolves: both lines kept, staged by hand — the exact
        // state `git cherry-pick --continue` asks for.
        std::fs::write(
            std::path::Path::new(&r.0).join("shared.txt"),
            b"from topic\nfrom main\n",
        )
        .expect("the resolution");
        r.git(&["add", "-A"]);

        g.cherry_pick_continue().expect("continue");
        assert!(!g.cherry_pick_in_progress(), "nothing left standing");
        assert_eq!(
            subjects(&r)[0],
            "topic-one",
            "the copy keeps its original subject"
        );
        let tree = g.status().expect("status");
        assert!(tree.staged.is_empty() && tree.unstaged.is_empty());
    }

    #[test]
    fn a_sha_spelled_like_a_flag_is_refused_before_any_process_runs() {
        let r = linear_repo("pick-dashes");
        let g = r.open();
        let head = r.rev_parse("HEAD");
        let err = g.cherry_pick(b"--exec=touch /tmp/x").unwrap_err();
        assert!(err.contains("refused"), "{err}");
        assert_eq!(r.rev_parse("HEAD"), head, "nothing ran");
        assert!(!g.cherry_pick_in_progress());
    }

    /// Two branches that each rewrote `shared.txt` their own way, ending on
    /// `main` — the least state a cherry-pick conflict means anything in:
    /// picking topic's commit onto main collides with main's own rewrite.
    fn two_branches_editing_one_file(name: &str) -> Scratch {
        let r = Scratch::new(name);
        r.write("shared.txt", b"base\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "base"]);
        r.git(&["checkout", "-qb", "topic"]);
        r.write("shared.txt", b"from topic\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "topic-one"]);
        r.git(&["checkout", "-q", "main"]);
        r.write("shared.txt", b"from main\n");
        r.git(&["add", "-A"]);
        r.git(&["commit", "-qm", "main-move"]);
        r
    }

    // ---------------------------------------------------------------- tags

    #[test]
    fn created_tags_read_back_whichever_kind_they_are() {
        let r = linear_repo("tag-create");
        let g = r.open();
        let tip = r.rev_parse("HEAD");
        let older = r.rev_parse("HEAD~2");

        // Lightweight at HEAD; annotated with a message riding stdin; and
        // one aimed at an explicit sha from history rather than HEAD.
        g.create_tag(b"v1", b"HEAD", None).expect("lightweight");
        g.create_tag(b"v2", b"HEAD", Some("release two\n"))
            .expect("annotated");
        g.create_tag(b"at-old", older.as_bytes(), None)
            .expect("at a sha");

        let got = g.tags().expect("tags");
        assert_eq!(
            branch_names_of_tags(&got),
            vec![b"at-old".as_slice(), b"v1".as_slice(), b"v2".as_slice()],
            "git's own order, by refname"
        );
        for name in [b"v1".as_slice(), b"v2".as_slice()] {
            let tag = got.iter().find(|t| t.name.as_bytes() == name).unwrap();
            assert_eq!(
                tag.commit,
                tip,
                "{} names the tip either way",
                String::from_utf8_lossy(name)
            );
        }
        assert_eq!(
            got.iter()
                .find(|t| t.name.as_bytes() == b"at-old")
                .unwrap()
                .commit,
            older,
            "an explicit sha is aimed at, never silently HEAD"
        );

        // Cross-checked against git's own picture of the refs, which is how
        // annotated and lightweight stay two different things and not just
        // two spellings of our parser: a lightweight tag's object *is* the
        // commit; an annotated one points at a tag object instead.
        let out = r.git_os_out(&[
            "for-each-ref".into(),
            "--format=%(refname:short) %(objecttype)".into(),
            "refs/tags".into(),
        ]);
        let kinds: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_string)
            .collect();
        assert!(kinds.iter().any(|l| l == "v1 commit"), "{kinds:?}");
        assert!(kinds.iter().any(|l| l == "v2 tag"), "{kinds:?}");

        // The annotated message arrived verbatim: `--file=-` stores stdin
        // byte-for-byte, where `-m` would append a newline of its own — a
        // mangled payload here would otherwise pass every check green. The
        // brackets fence off for-each-ref's own record separator.
        let note = r.git_os_out(&[
            "for-each-ref".into(),
            "--format=[%(contents)]".into(),
            "refs/tags/v2".into(),
        ]);
        assert_eq!(String::from_utf8_lossy(&note), "[release two\n]\n");

        // Deleting at trait level takes the name off and leaves every
        // commit it pointed at alone.
        g.delete_tag(b"v1").expect("delete");
        let got = g.tags().expect("tags after delete");
        assert_eq!(
            branch_names_of_tags(&got),
            vec![b"at-old".as_slice(), b"v2".as_slice()]
        );
        assert_eq!(subjects(&r).len(), 4, "deleting a name removes no commits");

        // A missing tag is git's refusal to give, verbatim enough to read.
        let err = g.delete_tag(b"nope").unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn a_duplicate_or_unnameable_tag_is_refused_with_the_words_that_act() {
        let r = linear_repo("tag-refuse");
        let g = r.open();
        g.create_tag(b"v1", b"HEAD", None).expect("first");

        // A duplicate is git's sentence, quoted whole — the reader decides
        // whether to pick another name or delete the old tag first.
        let err = g.create_tag(b"v1", b"HEAD", Some("again")).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        let err = g.create_tag(b"v1", b"HEAD", None).unwrap_err();
        assert!(err.contains("already exists"), "{err}");

        // Emptiness is ours, said beside the field that closed; dashes are
        // ours before any process runs. Both leave nothing behind.
        let err = g.create_tag(b"", b"HEAD", None).unwrap_err();
        assert!(err.contains("needs a name"), "{err}");
        let err = g.create_tag(b"-umalicious", b"HEAD", None).unwrap_err();
        assert!(err.contains("refused"), "{err}");
        let err = g.delete_tag(b"--whatever").unwrap_err();
        assert!(err.contains("refused"), "{err}");

        let got = g.tags().expect("tags");
        assert_eq!(branch_names_of_tags(&got), vec![b"v1".as_slice()]);
    }
}
