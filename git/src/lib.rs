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
use plait_core::status::{
    Change, ConflictEntry, ConflictKind, Kind, PathBytes, StagedEntry, Status, Submodule,
    UnstagedEntry, UntrackedEntry,
};
use plait_core::{parse_log, Commit, FileDiff};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;

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
    /// [`plait_core::status`] for the model and why the four are separate.
    fn status(&self) -> Result<Status>;

    /// A short label for the window title.
    ///
    /// Infallible: a repository whose branch cannot be read still has a name.
    fn describe(&self) -> String;
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
            run(&self.root, &[&["diff"], &RAW[..], &[revspec]].concat())?
        } else {
            // A bare revision means "what did this commit change".
            run(
                &self.root,
                &[&["show"], &RAW[..], &["--format=", revspec]].concat(),
            )?
        };

        let changes = parse_raw(&raw);

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
        out.extend(
            loose?
                .untracked
                .iter()
                .filter_map(|e| loose_pair(e, &self.root)),
        );
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
                .or_else(|| new_side(&c.new_oid, &self.root, c.path.as_bytes()));
            let binary = old.as_ref().is_some_and(|b| is_binary(b))
                || new.as_ref().is_some_and(|b| is_binary(b));
            // The lossy decode happens here and only here: everything above —
            // the record, the batch alignment, the working-tree read — went
            // through the raw bytes, so what reaches a frontend is the display
            // form of the path git actually named.
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
        // Bytes throughout: paths carry no encoding guarantee and the model
        // keeps them raw, so there is no decode here to get wrong.
        let raw = run(
            &self.root,
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
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
/// A free function over *any* [`Repo`], and deliberately not a method on the
/// trait: which lines correspond is decided by the configured registry and only
/// by it. A `Repo` implementation that answered with its own diff would make
/// `[diff] algorithm` a lie in every client at once, and rule 1 with it — the
/// differ an extension registers must be reachable through the same call. This
/// runs after acquisition, outside the trait, where no implementation can reach.
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
            false => FileDiff {
                path: p.label(),
                ..differs.file_using(over, &p.path, &p.old, &p.new)
            },
        })
        .collect())
}

// -------------------------------------------------------------------- status

/// `git status --porcelain=v2 -z`, parsed into the model in
/// [`plait_core::status`](plait_core::status).
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
        binary,
    })
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
/// NUL-terminated, and a rename or copy carries two of them. Anything that does
/// not start with `:` is skipped rather than guessed at — `git show` prefixes a
/// commit header that `--format=` does not always suppress.
///
/// Bytes end to end, with the same framing discipline as the porcelain v2
/// parser: every path slot is consumed exactly once, present or not, so one
/// malformed record can never shift the stream and rename somebody else's
/// file. A path is whatever bytes git emitted — no decode happens until a
/// [`Pair`] is built for display.
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
    if !is_null_oid(oid) {
        return None;
    }
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
fn lines(content: &[u8]) -> Vec<Arc<str>> {
    let text = String::from_utf8_lossy(content);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    if text.is_empty() && content.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .map(|l| Arc::from(l.strip_suffix('\r').unwrap_or(l)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    const SCRATCH_CONFIG: [&str; 6] = [
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "-c",
        "commit.gpgsign=false",
    ];

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("plait-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let me = Scratch(dir);
            me.git(&["init", "-q", "-b", "main", "."]);
            me
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
            "plait does not ask git for ignored files — target/ would be forty \
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
        assert_eq!(
            strs(&lines(b"a\r\nb\r\n")),
            ["a", "b"],
            "CRLF is not part of the line"
        );
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
