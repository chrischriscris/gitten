# 0032 — Blobs are shown by the renderer that can draw them

**Status** accepted
**Date** 2026-09

## Context

A PNG, a PDF or a tarball reaches every frontend the same way: a `Pair` with
`binary: true` and no lines on either side. The diff pane can say "binary" and
does, which is honest and useless — the file is *right there*, and the reader
who selected a screenshot wants to look at it.

Three things made this a decision rather than a feature:

- **Nothing here can decode a picture.** `core` has no dependencies, and the
  window's renderer does — `image`, plus SVG and a GPU atlas. Decoding in `gui`
  would mean depending on a second copy of what the renderer already links.
- **Nothing here can draw a PDF at all.** A page needs a renderer: `pdfium`,
  `mupdf` and `poppler` are C or C++ and all three are out on the dependency
  rule. macOS carries Quick Look, Linux usually carries poppler or ghostscript,
  and neither is a guarantee.
- **A viewer is where a naive design gets slow.** Decoding per frame, holding a
  gigabyte of video in memory, re-rasterizing a page on every resize — each is
  one line away at every step.

## Decision

**Bytes are read once, under a cap, off the render path.** `gitten-git` grew
`Repo::blob_pair`, the same `--raw` record the line-shaped reads use with both
bodies fetched through one `cat-file --batch`; `app::blobs::load` calls it and
rasterizes a first page if the bytes are a PDF. Both run on the client's job
thread. The cap is checked against the **size in cat-file's header**, before any
body is allocated, and a working-tree side is `stat`ed before it is read — so a
2 GB video is refused with a number rather than loaded to find out. A side over
the cap is `TooBig`, which is a different answer from `Absent`; conflating them
says "the file was added" about a video.

**Identity is content, and everything is keyed on it.** A blob never changes, so
an object-database side is keyed by its OID and a working-tree side by a hash of
its bytes. Two consequences, both wanted: the same picture at two revisions is
one entry in the store and one texture in the atlas, and a file rewritten
between two loads cannot be served from the previous answer — the key it was
stored under no longer exists. The store is bounded by bytes, not by count.

**The renderer decodes, not us.** The bytes go into a process-wide store, and
the pane draws them as an *asset path* — `gitten/blob/<key>` — served by the
crate's own `AssetSource`. GPUI fetches it once per key, decodes it on its own
executor with codecs we do not link, and caches the result in its own asset
cache; the frame is one `img()` element and a texture. The asset path carries no
scheme and the key no colon, deliberately: every asset system treats a
scheme-shaped string as a URL, and `blob:abc` would be a network fetch.

**A page is rasterized once per document side per page, at a fixed width.** The tool
is found by name on `PATH` — `pdftoppm`, `mutool`, `gs`, then Quick Look — never
by `cfg(target_os)`, which is plans/003's rule for external tools and the reason
a Mac with poppler gets poppler and a Mac without gets the one every Mac has. No
renderer at all is not an error: the bytes are still described, and the note
names what was looked for. The raster is stored as PNG bytes under the
document's identity, so a resize re-samples a texture instead of re-rendering a
document and a second look is a cache hit.

**Every tool runs under a deadline, and it is not optional.** A job queue is
single-threaded, so a renderer that does not return takes every later write in
the process with it — and one of them really does: Quick Look on a document it
cannot parse never exits. It was found the way it should have been, by the test
suite hanging, and it is killed past `PAGE_TIMEOUT` with the refusal saying so.
A presentation that cannot be drawn is *described*; a job that cannot be
finished is a window that stops responding, and that is not a trade this or any
other feature gets to make.

**Kinds are classified from magic bytes, in `core`.** A path is a claim by
whoever named the file; the header is the file. The classifier is pure, tested
and shared — which is what will let the terminal describe a blob the same way
the window draws one.

**Drawing is a registry.** `views::blob::Presenter` claims kinds and returns an
element; `image` and `hex` are registered through the same `register` an
extension would call, and the fallback claims everything, so a kind nobody has a
presentation for is *described* — size, OID, the first 64 rows of hex — rather
than blank.

## Why not X

- **A `Rows` implementation** (`presentations` in `extending.md`): row height is
  fixed for the whole list because `uniform_list` is the only reason a 714k-row
  diff scrolls. A picture is not one row of `ROW_H`, and a presentation that
  needs variable height wants a pane of its own. This is that pane.
- **Decoding in `gui` with the `image` crate**: a second copy of what the
  renderer links, a decode on whichever thread happened to be rendering, and a
  `RenderImage` whose lifetime we would then have to manage against the atlas
  cache the renderer already has.
- **Writing the blob to a temporary file and pointing `img()` at the path**:
  turns every glance at a picture into a disk write, and makes the cache key a
  path that no longer says what the content was.
- **`sips` for PDFs on macOS**: it renders at the page's own 72 dpi and cannot
  be asked for more, so a page of small print comes out unreadable.
- **Keeping the bytes in `Pair`** so no second read is needed: a thousand-file
  diff would hold a thousand blobs, and the diff pipeline's whole shape is that
  bytes become lines and are dropped.

## Evidence

- The scene of the bug: a 214 KB PNG selected in the rail, the centre empty,
  `+0 −0` in the header (the screenshot that opened this).
- The 10px gap in the *scrollbar* is the same class of mistake in miniature —
  see 0027 — and was found the same way, by measuring the screenshot rather than
  the code.
- The capped read is **14.5 ms** for a modified 200 KB PNG and **19.2 ms** for a
  5 MB one, release, in a scratch repository — the cost is git's two process
  spawns rather than the bytes, which is why the two are a third apart. Both
  figures, the harness and the Quick Look page time (166 ms) are in
  [blobs.md](../blobs.md#what-this-costs).
- Rasterizers on the development machine: `pdftoppm`, `mutool` and `gs` absent,
  `qlmanage` present — which is why the probe exists rather than a hard-coded
  tool, and why the "no renderer" note is not a corner case nobody meets.

## Consequences

- A blob over the store's budget (64 MB) is refused with its size. Raising it is
  one constant; the read limit and the store budget are deliberately the same
  number.
- One PDF rasterizer's output is a *thumbnail* — Quick Look's — so a page drawn
  by it is smaller than the width asked for. Poppler and ghostscript are asked
  for a page at 150 dpi instead.
- Only page one of a PDF is drawn, and each side draws its own — a modified
  PDF's "before" is the old document's raster, not the new one's again. The
  seam (`page_key(document, page)`) and the cache already carry the page
  number, so a page flipper is a control and a reload rather than a redesign.
  A renderer's answer is bounded the way the document was: the pipe is drained
  but only `OUT_CAP` bytes are kept, and the scratch file a path-shaped tool
  needs is a unique name written with `create_new`, so a shared `/tmp` cannot
  steer it through a planted link.
- The rust `image` codecs the renderer ships decide which image kinds can be
  drawn; the classifier names kinds the renderer might not have, and a kind it
  cannot decode draws as nothing. A fallback on decode failure is the obvious
  next step.
- Reading a *large binary* still costs the line-shaped diff read first, which
  reads both sides whole to decide `binary`. Git's own `core.bigFileThreshold`
  rule (a blob past a size is treated as binary without being read) is the fix,
  and it is acquisition's to make, not this feature's.
