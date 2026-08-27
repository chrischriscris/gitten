# Plan 003: The unified-patch parser reads coordinates, content, and paths correctly

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> "STOP conditions" item occurs, stop and report. When done, update this plan's
> row in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 3a8b347..HEAD -- core/src/lib.rs`
> If `core/src/lib.rs` changed, compare the excerpts against the live code; on a
> mismatch, STOP.

## Status

- **Priority**: P1
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `3a8b347`, 2026-08-24

## Why this matters

`core/src/lib.rs` parses an already-produced unified diff (a `.diff` file, piped
`git diff` output via `gitten diff -`, or the `--fixtures` demo). Three
independent parsing bugs make it mis-render real patches, all silently:

1. **Hunk header tail parsed as coordinates.** The header
   `@@ -10,4 +10,4 @@ const X: i32 = -1;` has a function-context tail. The parser
   scans *every* whitespace token and lets the trailing `-1;` overwrite the old
   line number with 0, so the whole hunk's gutter numbers are wrong.
2. **`--`/`++` content lines dropped.** A removed line whose source text starts
   with `-- ` (SQL, Lua, Haskell, Ada comments) appears in the patch as
   `--- comment` and is discarded by the file-header skip, which also fails to
   advance the line counter — so every following line in the hunk is numbered one
   too low, compounding.
3. **Paths with spaces become `?`.** The path is extracted from
   `diff --git a/… b/…` by whitespace position, so a path containing a space
   yields `"?"`, which routes the file to the fallback differ/highlighter/
   presentation (no syntax coloring, wrong view).

Each is a silent wrong-output bug on the patch-review path.

## Current state

`core/src/lib.rs`, the patch parser (lines 333-436):

```rust
// path extraction — BUG 3
if let Some(rest) = line.strip_prefix("diff --git ") {
    let path = rest
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.strip_prefix("b/"))
        .unwrap_or("?")
        .to_string();
    files.push(FileDiff { path, hunks: Vec::new() });
    continue;
}
if line.starts_with("@@ ") {
    let (o, n) = parse_hunk_header(line);   // BUG 1
    ...
}
// metadata skip — BUG 2: runs BEFORE content classification below
if line.starts_with("+++ ") || line.starts_with("--- ") || line.starts_with("index ") {
    continue;
}
let Some(hunk) = files.last_mut().and_then(|f| f.hunks.last_mut()) else {
    continue;
};
let (kind, text) = match line.as_bytes().first() {
    Some(b'+') => (LineKind::Added, &line[1..]),
    Some(b'-') => (LineKind::Removed, &line[1..]),
    Some(b' ') => (LineKind::Context, &line[1..]),
    _ => continue,
};
```

```rust
/// A hunk header split into the coordinates and the code around them.
/// EXISTS ALREADY and is correct — use it.
pub fn hunk_parts(header: &str) -> (&str, &str) {
    let Some(end) = header
        .find("@@")
        .and_then(|a| header[a + 2..].find("@@").map(|b| a + 2 + b + 2))
    else {
        return (header, "");
    };
    (&header[..end], header[end..].trim_start())
}

/// `@@ -41,9 +41,11 @@ ...` -> (41, 41)   -- BUG 1 lives here
fn parse_hunk_header(line: &str) -> (u32, u32) {
    let mut old = 0;
    let mut new = 0;
    for tok in line.split_whitespace() {          // scans the WHOLE line, tail included
        let num = |s: &str| s.split(',').next().unwrap_or("0").parse().unwrap_or(0);
        if let Some(s) = tok.strip_prefix('-') { old = num(s); }
        else if let Some(s) = tok.strip_prefix('+') { new = num(s); }
    }
    (old, new)
}
```

Conventions to match:
- `core` has **zero dependencies** — do not add a crate (no regex, no
  shell-words). Plain string ops only.
- Tests live in the `#[cfg(test)] mod tests` block at the bottom of the same
  file. Follow the existing naming style (long snake_case sentence names like
  `a_multibyte_line_is_measured_in_columns_not_bytes`).

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Test core | `cargo test -q -p gitten-core` | `test result: ok` |
| Lint | `cargo clippy -p gitten-core --all-targets` | no warnings |
| Format check | `cargo fmt --check` | clean |

## Scope

**In scope**:
- `core/src/lib.rs` — `parse_hunk_header`, the metadata-skip ordering, the
  `diff --git` path extraction, and new tests.

**Out of scope**:
- `git/src/lib.rs` — the `--raw` acquisition path is separate and already correct
  (it uses `-z` NUL separation). Do not touch it.
- `hunk_parts` — it is correct; call it, don't rewrite it.
- Full C-style path dequoting is OPTIONAL (see Step 3); the space-splitting fix
  is required, dequoting is a stretch goal, gate it as noted.

## Git workflow

- Branch: `advisor/003-patch-parser-hardening`
- One commit per bug is fine, or one commit for all three; imperative messages.

## Steps

### Step 1 (BUG 1): Parse coordinates only from the header's coordinate section

Rewrite `parse_hunk_header` to scan only the part before the second `@@`:

```rust
fn parse_hunk_header(line: &str) -> (u32, u32) {
    let (coords, _tail) = hunk_parts(line);
    let mut old = 0;
    let mut new = 0;
    for tok in coords.split_whitespace() {
        let num = |s: &str| s.split(',').next().unwrap_or("0").parse().unwrap_or(0);
        if let Some(s) = tok.strip_prefix('-') {
            old = num(s);
        } else if let Some(s) = tok.strip_prefix('+') {
            new = num(s);
        }
    }
    (old, new)
}
```

**Verify**: `cargo build -p gitten-core` → exit 0.

### Step 2 (BUG 2): Only skip metadata lines before the first hunk

The `+++ `/`--- `/`index ` lines only ever appear in a file's header, which is
always before its first `@@`. So the skip is only safe when the current file has
no hunk yet. Move the skip below the hunk lookup, or guard it. The cleanest fix:
reorder so the hunk guard runs first, then skip metadata only when there is no
current hunk. Replace the skip + guard block with:

```rust
let Some(hunk) = files.last_mut().and_then(|f| f.hunks.last_mut()) else {
    // Before any hunk of the current file: the metadata lines live here, and
    // there is nothing to attach a content line to anyway.
    continue;
};
```

Deleting the explicit metadata-skip block entirely is correct: any `+++`/`---`/
`index` line appears before the first `@@`, at which point `hunks.last_mut()` is
`None` and the line is skipped by the guard above. Once inside a hunk, a line
starting with `-` or `+` is genuine content and must be classified, not skipped.

Double-check: the `diff --git` and `@@ ` branches `continue` before reaching this
guard, so they are unaffected.

**Verify**: `cargo build -p gitten-core` → exit 0.

### Step 3 (BUG 3): Extract the path by the `a/…b/…` structure, not whitespace

Replace the `diff --git` path extraction. `git diff --git` prints
`a/<old> b/<new>`; the new path is what follows the last ` b/`. For unquoted
paths:

```rust
if let Some(rest) = line.strip_prefix("diff --git ") {
    // `a/<old> b/<new>`; a path may contain spaces, so split on the ` b/`
    // boundary rather than on whitespace. The new path is what we render.
    let path = rest
        .rfind(" b/")
        .map(|i| rest[i + 3..].to_string())
        .unwrap_or_else(|| "?".to_string());
    files.push(FileDiff { path, hunks: Vec::new() });
    continue;
}
```

OPTIONAL stretch goal — git quotes paths with unusual bytes as `"a/has\tspace"`
unless `core.quotePath=false`. If `rest` starts with `"`, both sides are quoted;
handling that needs a small dequote. **Only** attempt this if you can write a
test that passes; otherwise leave a `// TODO: dequote git's "…" path form` and
STOP-report that you deferred it. Do not ship a half-working dequoter.

**Verify**: `cargo build -p gitten-core` → exit 0.

### Step 4: Tests

Add tests in the `#[cfg(test)] mod tests` block. Find the existing patch-parsing
test (grep for `parse` or `diff --git` or `@@` in the test module) and model on
it. Cover:

- **BUG 1**: a hunk header `@@ -10,4 +10,4 @@ const X: i32 = -1;` yields
  `old == 10, new == 10` (not 0). Also test a header with a leading-`-` bullet in
  the tail (`@@ -5,2 +5,2 @@ - item`).
- **BUG 2**: a hunk removing a line whose text is `-- a comment` produces a
  `Removed` line with text `- a comment` (one `-` stripped), and the next line's
  `old_no` is correctly incremented (not off by one).
- **BUG 3**: `diff --git a/dir with spaces/a.rs b/dir with spaces/a.rs` yields a
  `FileDiff` whose `path` is `dir with spaces/a.rs`, not `?`.

**Verify**: `cargo test -q -p gitten-core` → all pass, including the 3+ new tests.

## Test plan

- 3–4 new tests in `core/src/lib.rs`, one per bug plus the bullet-tail edge.
- Each test feeds a small literal patch string into the parser entry point and
  asserts on the resulting `Vec<FileDiff>` (line numbers, line text, path).
- Verification: `cargo test -q -p gitten-core` → all pass.

## Done criteria

ALL must hold:

- [ ] `cargo test -q -p gitten-core` exits 0; the new tests exist and pass
- [ ] `grep -n "hunk_parts" core/src/lib.rs` shows `parse_hunk_header` now calls it
- [ ] `grep -n "starts_with(\"--- \")" core/src/lib.rs` returns nothing (metadata skip removed/relocated)
- [ ] `grep -n "rfind(\" b/\")" core/src/lib.rs` shows the new path extraction
- [ ] `cargo clippy -p gitten-core --all-targets` clean; `cargo fmt --check` clean
- [ ] No files outside `core/src/lib.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- The parser's structure differs materially from the excerpt (drift) — report it.
- Removing the metadata skip breaks an existing test — that would mean a real
  patch relies on the old behavior; report the failing test rather than
  re-adding the skip.
- You cannot write a passing dequote test for the quoted-path stretch goal —
  leave the TODO and report; do not ship a partial dequoter.

## Maintenance notes

- The `--raw` acquisition path in `git/src/lib.rs` does not share this parser and
  is already space-safe via `-z`; these fixes are specific to consuming
  pre-rendered patches.
- A reviewer should check that classification of `-`/`+` content lines now
  happens for every in-hunk line (BUG 2's core risk is over-skipping) and that
  the line counters advance for exactly the lines that are kept.
