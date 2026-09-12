# Blobs: seeing what has no lines

A file in a diff is two lists of lines. A PNG, a PDF and a tarball have none —
git calls them binary and stops — so a viewer that only knows how to diff has
nothing to draw. This is the road those bytes take instead.

```
  1  classify   core::blob::Kind::of          what the first 8000 bytes are
  2  acquire    Repo::blob_pair               one raw record + one cat-file --batch
                 a cap is checked from the header's size, before any body
  3  rasterize  app::blobs::Rasterizer        a PDF's first page, one tool, on PATH
  4  store      core::blob::Store             OID- or content-keyed, bounded in bytes
  5  serve      gui::assets::Assets           `gitten/blob/<key>` → bytes, on demand
  6  draw       views::blob::Presenter        one `img()`, or a hexdump, or a sentence
```

Stages 1–4 run on a job thread — `cx.background_spawn`, the same lane the file
preview uses — and only when the selection's diff came back with **one file and
no hunks**, which is the one shape a diff cannot draw and a viewer might. A text
diff pays nothing: no process, no lookup, no branch on the render path.

## 1. What the bytes are

`Kind::of` reads magic bytes: PNG, JPEG, GIF, WebP (the `RIFF`/`WEBP` pair, not
the `RIFF`), BMP, ICO, TIFF, PDF (a scan for `%PDF-`, because the format allows
a prologue), SVG (the *root element* — an HTML page with an inline `<svg>` is a
page), text (no NUL and valid UTF-8) and `Opaque` for the rest.

The extension is not consulted. `logo.png` holding a JPEG is what renaming a
`.gif` produces, and a viewer that trusts the name draws nothing where it could
have drawn a picture. The classification is a pure function of the bytes, which
is why it is in `core` and not in the window: the terminal will want to say the
same thing about the same file.

## 2. The read, and its cap

`Repo::blob_pair(source, path, cap)` runs the same record the line-shaped
reads use, per source: a bare revision (`Commit`, `Stash`, a revspec naming
one) is `git show` so a merge answers and a commit's sides are parent →
commit — never the worktree the file may have moved on to — a range is `git
diff`, the aggregate is `HEAD` → worktree, and a rename, an unborn branch,
an untracked file and a conflict are all the same problem they are
everywhere else. Bodies come through **one** `cat-file --batch`. One process
for the pair, whatever the pair is, for the reason the diff reader gives: a
fork per file is a second before any work happens.

Three things about the cap, and all of them are the point of it:

- **It is spent before the body is read.** `cat-file --batch` reports each
  object's size in its header, so a 2 GB video is refused from twelve bytes of
  answer. What that costs is a *drain* — git is already writing the body into
  the pipe, and closing our end mid-body is a git killed by `SIGPIPE` reported
  as a failure for a file that is merely large — so the bytes are read and
  thrown away in 64 KB chunks, on the one path where somebody asked for a file
  they were about to be warned about.
- **A working-tree side is a regular file or it is nothing.** `metadata`
  follows links, so a symlink to a device (`len` 0, a read that never ends)
  or a fifo (a read that never starts) is `Absent`, not a hang of the one job
  thread every later write shares. A link to an ordinary file still reads the
  file it names.
- **The read is capped too, not just the stat.** A file that grows past the
  limit between the two is refused, not loaded whole — and the refusal's size
  is re-stat'ed on the open descriptor, so the number reported is the file
  that is.

`TooBig` is a distinct answer from `Absent`, and it carries the size. Conflating
them says *the file was added* about a video.

## 3. A PDF's page

There is no PDF renderer in this tree and there will not be one: `pdfium`,
`mupdf` and `poppler` are C or C++, and the dependency rule is not worth a
document viewer. So a page comes out of whichever tool this machine has:

| tool | shape | why this one |
|---|---|---|
| `pdftoppm` | prefix output | poppler's own; the reference page |
| `mutool` | stdin → stdout | mupdf; same quality, one pipe |
| `gs` | stdin → stdout | everywhere, slower |
| `qlmanage` | directory thumbnail | every Mac has it; a *thumbnail*, so the page is fitted into a square at the width asked for |

Found **by name on `PATH`**, never by `cfg(target_os)` — plans/003's rule for
external tools, and the reason a Mac with poppler installed gets poppler's page
rather than Quick Look's. The probe runs once per process and caches its answer.
No tool at all is not a failure: the pane draws the bytes' own description and
the note names what was looked for.

The raster is made **once per document per page, at a fixed width** (`PAGE_PX`,
1400) — and **once per side**: a modified PDF flipped to "before" draws the old
document's own raster, not the after one's again. A raster that depended on the
pane's width would be re-made on every resize, and a resize is a frame; a fixed
one is re-sampled by the platform instead, which is free.

The answer is bounded the way the document was (`OUT_CAP`, 64 MB): the pipe is
drained to the end so the child can always exit, but only the cap is kept — a
page-sized answer that overflows it is a document attacking its reader, refused
rather than held. And the path-shaped half writes to a name nobody could have
planted: a per-process counter behind the pid, created with `create_new`, so a
shared `/tmp` cannot turn a scratch write into a write through somebody's link.

**Every tool runs under a deadline** (`PAGE_TIMEOUT`, 20 s), and it is not a
performance budget — it is liveness. These are system services and command-line
tools invoked on a client's job thread, and a job queue is single-threaded: a
renderer that does not return takes every later write in the process with it.
This is not hypothetical. Quick Look on a document it cannot parse **does not
exit**, measured on the development machine — the first version of this code sat
in `qlmanage` until the test suite was killed by hand. A renderer past its
deadline is killed and the failure says so, which is a different sentence from a
renderer that answered with nothing.

## 4. The store, and the identity everything is keyed on

A blob never changes, so its identity is its content: the OID where git gave one
and a fast hash of the bytes where it did not (a working-tree side is nowhere
but disk). Two things fall out of that, and both are wanted:

- the same picture at two revisions is one entry, one decode and one texture;
- a file rewritten between two loads **cannot** be served from the previous
  answer — the key it was stored under no longer exists. A path-keyed cache gets
  this wrong and shows a stale picture, which is the kind of bug that looks like
  the app forgetting to refresh.

The store is bounded in **bytes** (64 MB), not entries: one video and one favicon
are not the same memory. Eviction is insertion order, because a viewer's working
set is the file under the cursor and the one it came from — the interesting
question is whether the *last* few are here, and an LRU's bookkeeping costs more
than it saves on a list this short.

A key contains **no colon**. A frontend hands it to a renderer's asset system as
a path, and every such system treats a string with a scheme as a URL — `blob:abc`
is not a blob, it is a fetch. `blob-<oid>` and `content-<hash>-<len>` say the same
thing and mean nothing to a URI parser.

## 5. Serving it, and who decodes

`gitten/blob/<key>` is a third arm in the shell's `AssetSource`. The renderer
asks for it, the source answers from the store, and **the renderer decodes** —
its own codecs, on its own executor, cached against that path. This is the whole
performance story of the feature in one sentence: a frame of an image is one
`img()` element built from a key that is already a hit, one texture the platform
draws, and no pixel in this process's render path.

It also makes the store and the asset source one thing per process rather than
per window, which is why the store is a `OnceLock`: GPUI installs one asset
source per application, and a window-local store would be a window whose
pictures the renderer could not cache.

## 6. Drawing, as a registry

```rust
pub trait Presenter {
    fn name(&self) -> &'static str;
    fn claims(&self, kind: Kind) -> bool;
    fn render(&self, what: &Shown, host: &Host, store: &Store) -> AnyElement;
}
```

Two built-ins, both through `Presenters::register` — the call an extension would
make: `image` claims the kinds the renderer can decode and draws one `img()` with
`ObjectFit::Contain` (the platform's sampler is the entire scaling policy), and
`hex` claims everything and draws the head of the blob in hex and text. The
fallback is what makes a kind nobody has a presentation for *described* rather
than blank, and it is bounded at 64 rows so a thousand-megabyte file costs the
same as a ten-byte one.

The pane is `views::blob::Blob`, and the shell picks it as the centre's body when
the selection is a blob:

```
   workspace_body:  [ header: path, status, totals ]  [ body: diff rows | blob pane ]
                                                            ▲
                                        one slot, two bodies — the selection decides
```

`b` (bound to `blob.flip`) swaps between what the file *is* and what it *was*;
the pane's own strip carries the same two words as buttons, because a mouse is
not a second citizen. The command is a no-op on a text diff and on a file that
was only added.

## What this costs

Measured on the development machine, release, against a scratch repository with
a modified file in it — the shape a selection makes:

| | |
|---|---|
| a 200 KB PNG, `blob_pair` | **14.5 ms** |
| a 5 MB binary, `blob_pair` | **19.2 ms** |
| a PDF's first page, Quick Look | **166 ms** |

The two reads are that close because the cost is **git's two process spawns** —
one `git diff --raw` for the object ids, one `cat-file --batch` for the bodies —
and not the bytes: twenty-five times the content costs a third more time. A
*second* visit to the same blob is a store hit and no process at all.

All of it is on a job thread and only for a file whose diff came back with no
hunks, so a text diff pays nothing: no process, no lookup, no branch on the
render path.

Reproduce: `git/examples/blobtime.rs` (release) over a scratch repository with a
binary file modified on disk, or `time qlmanage -t -s 1600 -o DIR FILE.pdf`.

A blob over the cap is refused with a number, and the store over its budget
evicts the oldest.

## Gaps, named

- **Page one only.** The key shape carries the page number already, so a flipper
  is a control and a reload.
- **A kind the renderer cannot decode draws as nothing.** The classifier names
  kinds the `image` crate might have been built without, and the failure is
  silent — an `img()` with no pixels. A decode failure falling back to `hex`
  is the next step.
- **The cap is on compressed bytes, not decoded pixels.** A small PNG can
  inflate to hundreds of megabytes of texture — the only ceiling is the
  decoder's own limit. Sniffing dimensions out of the header and refusing
  absurd geometries is the fix, and it is a cheap one: `IHDR` sits at a fixed
  offset.
- **The binary *test* is still the whole-file read.** A large binary selected
  for the first time is read by the diff read before this one sees it; git's own
  `core.bigFileThreshold` rule is the fix, and it belongs to acquisition.
- **A stash's untracked half has no blob yet.** A file parked by `stash -u`
  lives in the stash commit's third parent, which `git show` on the commit does
  not name — the pane says nothing for it. `ls-tree` on that parent is the fix,
  the same read `pairs_stash_untracked` already makes for lines.
- **No zoom, no pan, no onion skin.** Two-up and swipe are the shapes a real
  image diff wants; this shows one side at a time, which is the honest first
  cut and not a claim that it is enough.
