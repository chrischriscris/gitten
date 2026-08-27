# Plan 011: Author colours read the live theme

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 2dfcb82..HEAD -- shell/src/views/commits.rs core/src/theme.rs`
> If either file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none (softly: land after plans/010-widest-row-in-characters.md,
  which edits the same file first)
- **Category**: bug
- **Planned at**: commit `2dfcb82`, 2026-08-26

## Why this matters

Pressing the theme picker (`T`) recolours the entire window — chrome, diff,
graph gutter — but every author chip keeps its previous palette until relaunch.
The colour is resolved once in `Commits::new` and stored into a per-commit
struct, while everything else on the same rows reads the live host every frame.
That is precisely the captured-clone mistake this crate documents as wrong:
`shell/src/config.rs:73-80` says views must call config/theme accessors *on the
render path* rather than hold one. It also silently breaks `[theme].authors`
table edits in the flagship client's main view.

## Current state

- `shell/src/views/commits.rs` — the GPUI commit column.
  - The capture, in `Commits::new`, :65–71:
    ```rust
    let who: Vec<Who> = commits
        .iter()
        .map(|c| Who {
            initials: initials(&c.author).into(),
            color: rgb(host.theme.author(&c.author)),
        })
        .collect();
    ```
  - The type it fills (:12–15):
    ```rust
    /// The commit column between the sha and the graph, resolved once at load: two
    /// letters and the colour they are drawn in. Not a per-frame job.
    struct Who {
        initials: SharedString,
        color: Rgba,
    }
    ```
  - Consumption in `row()` at :184: `.text_color(who.color)`. **Crucially, the
    very same function already receives the live host**: `fn row(c: &Commit,
    who: &Who, d: &graph::Draw, host: &Rc<Host>)` — it uses
    `host.font.char_width()` (:168) and `host.theme.chrome.dim` (:177). So no
    new plumbing is required; only the resolution point moves.
  - The batch closure that calls `row()` reads `crate::config::host(cx)` fresh
    per frame at :126, with its own pointer-comment ("Read per batch, not
    captured at construction … what makes a saved config apply on the next
    frame").
  - Note the sha column right beside it does it correctly:
    `.text_color(rgb(host.theme.chrome.dim))` at :177.
- `core/src/theme.rs:583-591` — `Theme::author(&self, author: &str) -> Rgb`:
  stable per-name via a small byte-fold hash into a palette slice, falling back
  to `chrome.dim` when the table is empty. Deterministic, pure, cheap (nanoseconds
  for a short name).
- Conventions exemplar for "resolve on the render path": the diff view keeps
  zero theme state and reads `host.theme…` inside each frame's closures — see
  the same pattern used by `Commits::render`'s scrollbar gate at :143–146
  (`crate::config::host(cx).view.scrollbar`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Crate tests | `cargo test -p gitten-shell` | all pass |
| Full gate | `./check.sh` | exit 0 |

## Scope

**In scope** (the only file you should modify):
- `shell/src/views/commits.rs`

**Out of scope**:
- `core/src/theme.rs` (the palette/hashing logic is correct and tested there).
- Any caching/memoisation structure for the colour lookup — see Step 3 for why
  it stays out.
- TUI/web authors-colour rendering (their loops re-resolve from their own
  config copies by construction).

## Git workflow

- Branch: `advisor/011-author-colours-live-theme`
- Commit style: sentence-case imperative like `Config lives in ~/.config, with
  a cwd override`. Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Shrink `Who` to just the letters

Remove the `color` field so the struct holds only what is genuinely
load-time-expensive to compute:

```rust
/// The two letters between the sha and the graph, resolved once at load. Not
/// the colour: that follows the live theme like everything else on the row.
struct Who {
    initials: SharedString,
}
```

Update `new` accordingly (:65–71): keep `initials`, drop `color`.

**Verify**: `cargo build -p gitten-shell` → exit 0.

### Step 2: Resolve the colour where the row draws

In `row()`, replace `.text_color(who.color)` (:184) with:

```rust
.text_color(rgb(host.theme.author(&c.author)))
```

and update the call-site's surrounding comments if any reference the stored
colour. Adjust `Data`/field docs that say the whole column is "resolved once at
load" — after this step only the initials are.

**Verify**: `cargo test -p gitten-shell` → all pass; `./check.sh` → exit 0.

### Step 3: Write down why no cache exists here

Add a short comment above the new `.text_color` line noting the deliberate
cost: one byte-fold hash per visible row per frame (`theme.author` is O(name)
with a constant palette modulo), measured below the frame budget and below the
already-present `char_width` work; a memo `HashMap` keyed by author name is the
prescribed follow-up **only if** profiling ever shows it. This satisfies the
AGENTS.md render-path allocation rule honestly rather than ritually.

**Verify**: `grep -n 'byte-fold' shell/src/views/commits.rs` → 1 hit.

## Test plan

The behaviour (recoloring on hot reload) needs a window; headless assertions
cannot observe it today, and inventing GPU-render scaffolding is out of scope
(only two `#[gpui::test]`s exist, both for pixel bounds in `split.rs`). What IS
machine-checkable:

- `cargo test -p gitten-shell` green: proves no regression in row assembly and
  confirms `Who` construction sites are all updated (the compiler enforces both).
- Structural greps in Done criteria prove the captured-clone shape is gone.

Manual reviewer checklist (documented, not automated): run `./dev desktop
commits`, press `T` twice — expected: author chips recolour with the window on
both toggles.

## Done criteria

All must hold:

- [ ] `grep -n 'color: rgb(host.theme.author' shell/src/views/commits.rs` finds the capture site gone AND no `Who {` literal still sets a colour
- [ ] `grep -n 'text_color(who.color)' shell/src/views/commits.rs` returns nothing
- [ ] `cargo test -p gitten-shell` exits 0
- [ ] `./check.sh` exits 0
- [ ] No files outside the in-scope list modified (`git status --short`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `row()` no longer receives a `Host` parameter at :167 (it would mean callers
  changed shape since the plan was written).
- You discover `author` colours participate in selection/copy output or tests
  snapshotting exact RGBs (changing snapshot expectations is not this plan).
- The drift check shows `theme.rs` moved the `author` method.

## Maintenance notes

- The settings-panel/registry work (roadmap A#2 and later) should keep treating
  theme lookups as per-frame data reads; do not reintroduce cached palettes as
  part of that migration.
- Reviewer focus: no `RefCell<HashMap>` sneaks in "for performance" without the
  measurement the added comment demands.
