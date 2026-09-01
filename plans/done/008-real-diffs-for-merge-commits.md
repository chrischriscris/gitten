# Plan 008: Render a real diff for merge commits

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 2dfcb82..HEAD -- git/src/lib.rs`
> If the file changed since this plan was written, compare the "Current state"
> excerpts against the live code before proceeding; on a mismatch, treat it as
> a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `2dfcb82`, 2026-08-26

## Why this matters

Selecting any **merge commit** in the log renders nothing (or worse, fabricated
data), silently. This repo's flagship fixture is git/git with 26% merges, so
this is not a corner case. Measured fact: on git 2.55,

```
git show --raw -z -M --abbrev=64 --format= --no-ext-diff <merge-sha>
```

(the exact invocation `pairs()` builds) emits **zero records** for a merge,
because modern git suppresses merge diffs unless asked otherwise. Older gits
and `[diff] diffMerges` configurations instead emit **combined-format** records
prefixed with N colons (`::100644 … MM`), and the parser below decodes those
into garbage OIDs and garbage status letters rather than refusing them.

The fix asks git for a first-parent diff of merges — ordinary, single-old/
single-new records the existing parser handles unchanged — and makes the parser
reject any combined record that still slips through, instead of mis-decoding it.

## Current state

- `git/src/lib.rs` — the whole acquisition layer; free functions over spawned
  `git`. Nothing here knows about UI.
  - `pub fn pairs(repo: &Path, revspec: &str) -> Result<Vec<Pair>>` at :151.
  - The raw-record flags constant, :201:
    ```rust
    const RAW: [&str; 5] = ["--raw", "-z", "-M", "--abbrev=64", "--no-ext-diff"];
    ```
  - Three invocations at :202–220. A bare revision (a clicked commit) goes
    through `git show`:
    ```rust
    // A bare revision means "what did this commit change".
    run(
        repo,
        &[&["show"], &RAW[..], &["--format=", "--end-of-options", revspec]].concat(),
    )?
    ```
    The range branch (:204–208) and the working-tree branch (:202–203) are
    **immune** — only the `show` path sees merge commits.
  - The parser, :517–553 — exactly one leading colon stripped, then five fixed
    positional slots:
    ```rust
    fn parse_raw(raw: &str) -> Vec<Change> {
        ...
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
            ...
            old_mode: parts[0].trim_start_matches(':').to_string(),
            new_mode: parts[1].to_string(),
            old_oid: parts[2].to_string(),
            new_oid: parts[3].to_string(),
        }
    ```
    A combined record (`::100644 100644 100644 abc def ghi MM`) has more fields:
    the slots shift, `old_oid` receives the mode string `"100644"`, `status`
    becomes a hex digit, and the wrong blob lands in `new_oid`. Downstream,
    `fetchable(mode, oid)` at :560–562 only checks GITLINK/null-OID, so the
    junk OID reaches `cat-file --batch`, gets `missing` back, maps to `Ok(None)`
    — and the user sees a plausible-looking one-sided diff drawn from whatever
    blob happened to land in slot 3. No error anywhere.
  - The doc comment above the parser (:513–516) frames skipping as "anything
    that does not start with `:`" — a combined record starts with `:` and walks
    straight through.
- Tests: `mod tests` at :769 onward, several building throwaway repositories
  with real `git` commands and running `parse_raw`/`pairs` over them (~23 test
  functions). None exercises a merge or a `::`-prefixed record.
- Repo conventions to match: lossy UTF-8 everywhere (`String::from_utf8_lossy`
  at :222, never `read_to_string` of git output); plain `Result<T, String>`
  style errors; comments explain *why* in prose. Match the surrounding test
  style in `mod tests`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Crate tests | `cargo test -p gitten-git` | all pass |
| Full headless gate | `./check.sh` | exit 0 (fmt + clippy -D warnings + all crates + fixtures) |
| Git feature probe | `git --version` then the probe in Step 1 | version ≥ 2.31; probe prints clean single-colon records |

## Scope

**In scope** (the only files you should modify):
- `git/src/lib.rs`

**Out of scope** (do NOT touch, even though they look related):
- `core/` (nothing downstream learns that a merge happened — status letters are
  shared data, not a new `LineKind`)
- Any shell/tui/web renderer. First-parent diffs are ordinary diffs to them.
- Rename detection flags, `cat-file --batch` logic, the untracked/status path.

## Git workflow

- Branch: `advisor/008-real-diffs-for-merge-commits`
- Commit per logical unit; message style matches the repo's history — sentence
  case, imperative, e.g. `Stop a revspec from being read as a git option`
  (commit `e25056b`). Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Ask git for first-parent diffs on the bare-revision path

In the `show` branch at :211–219, append `--diff-merges=first-parent` to the
argument list (before `--end-of-options`):

```rust
&[&["show"], &RAW[..], &["--format=", "--diff-merges=first-parent", "--end-of-options", revspec]]
```

Probe your environment's git supports it (it exists since 2.31):

```sh
git show --raw -z -M --abbrev=64 --format= --no-ext-diff \
    --diff-merges=first-parent $(git rev-parse HEAD) | head -c 200
```

Expected: zero-byte output for a root/first-parent-equal merge is fine, but a
merge whose result differs from parent 1 prints clean single-colon records
(`:100644 100644 … M\0path\0`). If your git errors on the flag, see STOP.

Update the comment above the invocation to say *why*: modern git emits no diff
for merges by default, and where older git/config emit combined records the
parser below refuses them — first-parent is the honest ordinary answer.

**Verify**: `cargo build -p gitten-git` → exit 0.

### Step 2: Refuse combined records in `parse_raw`

After the `strip_prefix(':')` at :521 succeeds, add one rejection: if the
remainder still starts with `:`, it is a combined record — `continue`. Update
the doc comment :506–516 to document both behaviours (single colon consumed;
two or more colons refused, with the reason: a combined record carries N modes,
N OIDs and an N-letter status, and decoding it positionally fabricates data).

A surgical way to say it:

```rust
let Some(meta) = meta.rsplit('\n').next().and_then(|m| m.strip_prefix(':')) else {
    continue;
};
// A second leading colon marks a combined record: N modes, N oids and an
// N-letter status that this fixed-slot parser would read as garbage. With
// --diff-merges=first-parent git cannot send one; refuse rather than decode.
if meta.starts_with(':') {
    continue;
}
```

**Verify**: `cargo test -p gitten-git` → all pass (Step 3 adds the proof).

### Step 3: Regression tests

Add to `mod tests` (model each on whichever nearby test fits; read the helpers
at the top of the module first):

1. **Parser unit test** — feed `parse_raw` a literal containing a combined
   record, e.g. `"::100644 100644 100644 aaaa bbbb cccc MM\0src/main.rs\0"`
   mixed with one well-formed `:100644 … M\0keep.txt\0` record. Assert the
   result contains exactly the well-formed entry, and every field of the
   combined record is absent.
2. **End-to-end merge test** — in the style of the scratch-repository tests in
   the module: create a repo, make branch base → commit A on main, branch off,
   commit B, merge (--no-ff, guaranteeing a true merge commit), resolve
   trivially so the result differs from parent 1. Then
   `pairs(&repo, "<merge-sha>")` must return a **non-empty** `Vec<Pair>` whose
   paths match `git diff --raw --no-ext-diff <merge-sha>^1 <merge-sha>` (parse
   git's own record list and compare path sets). Assert every status letter is
   one of `AMDR` (plus any letter git actually emitted) — i.e. assert
   `c.status.chars().next()` is alphabetic, which a hex-garbage decode would fail.

**Verify**: `cargo test -p gitten-git` → all pass including the two new tests
(output lists them by name).

### Step 4: Full gate

**Verify**: `./check.sh` → exit 0.

## Test plan

- Parser refusal test (happy path + specific regression this plan fixes).
- Scratch-repo merge test covering: two-parent ordinary merge; assertion that
  paths match first-parent raw output.
- Structural pattern: existing inline `#[test]`s in `git/src/lib.rs` `mod tests`
  (:769+) that build repos with `Command::new("git")` — copy their setup style.
- `./check.sh` green, including fmt/clippy gates.

## Done criteria

All must hold:

- [ ] `grep -n '\-\-diff\-merges=first\-parent' git/src/lib.rs` finds the show-path invocation
- [ ] `grep -n 'starts_with(':'` in `parse_raw` region shows the combined-record rejection
- [ ] `cargo test -p gitten-git` exits 0 with the two new tests named in output
- [ ] `./check.sh` exits 0
- [ ] No files outside the in-scope list are modified (`git status --short`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Your git exits non-zero on `--diff-merges=first-parent` (pre-2.31). Report the
  `git --version`; do not fall back to `-m`, whose pre-2.31 semantics emit one
  diff per parent and duplicate every changed path.
- The tests module's helpers cannot express a merge commit in ≤40 lines of setup
  (say what's missing rather than inventing infrastructure).
- Live `git diff --raw` disagrees with what Step 1 makes `pairs()` return on
  more than path spelling/quote-escaping grounds.
- The parser at :517 does not match the excerpt above.

## Maintenance notes

- If the roadmap's porcelain-v2 status model lands, first-parent diffs of
  merges stay correct; a future "show the actual combined result" mode belongs
  behind the same seam as a deliberate new surface, not by loosening this
  parser.
- Reviewer focus: the `continue` guard placement (it must sit *after* stripping
  one colon, *before* any field access) and that the range/working-tree branches
  gained no flags.
- Deferred deliberately: rendering "merge versus each parent" toggle, and
  handling explicit `[diff] diffMerges` configs pointing elsewhere than our
  flag — our flags override config, which is why Step 1 works at all.
