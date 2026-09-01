# Plan 048: `/` searches every list pane

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update this plan's row in
> `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `grep -n "fn row_bar" shell/src/views/diff.rs`
> must hit (the design pass is on your base). Line refs were taken at
> `00842dc` + the staged design pass; match on quoted content where a ref
> drifted; STOP on a structural mismatch.
>
> **Build cost**: `export CARGO_TARGET_DIR=/tmp/gitten-target`. Never launch
> `./dev desktop` or `./dev tui`.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW (the hard plumbing already exists for commits)
- **Category**: feature (parity) — three panes lack the verb the fourth has

## Why this matters

The commits pane has a live filter: `/` is bound to `commits.search`
(`core/src/command.rs:463`), a prompt opens, every edit filters the list
live, the pane label shows `12/4173`, accept keeps the filter and cancel
clears it. Branches, files and stashes have nothing — and the branches pane
is where it hurts: sixteen locals, eleven of them machine-named
`worktree-agent-*`, and the only way to reach `main` is to arrow past them.
`begin_search` even says so out loud: any other pane gets the notice
"commits.search is not supported here" (`shell/src/main.rs:2754`).

A filter is a property of a *list*, not of the commits pane. One mechanism,
every pane — which is also what puts the verb one keypress away in whatever
pane an extension registers.

After this plan: `/` opens the same live-filter prompt over files, branches
and stashes; each pane's label carries the `shown/loaded` note while
filtered; the cursor behaves exactly as the commits filter already does
(stays on its row when possible, clamps when not, disarms questions).

## Current state

- The exemplar, `shell/src/views/commits.rs`:
  - `filter: Option<String>` + the filtered index, "rebuilt only when the
    query changes — never per frame" (`commits.rs:64-68`);
  - "the filter narrows what is *shown*, never what is loaded"
    (`commits.rs:145`); every read goes through one accessor so "filtering
    cannot desync" (`commits.rs:166`);
  - `filter_note()` → `"12/4173"` (`commits.rs:178-184`);
  - `set_filter` "once per keystroke, and never anywhere else"
    (`commits.rs:392-410`), including the clamp + disarm rules;
  - tests from `commits.rs:1177` — `a_query_filters_live_and_the_keyboard_stays_on_its_commit`.
- The prompt plumbing in `shell/src/main.rs`: `begin_search`
  (`main.rs:2752-2760`) builds an `input::Input`, rejects non-commits panes
  (`:2754`); live edits route to the pane (`main.rs:2741` — "every edit
  filters that pane's list live"); accept/cancel at `main.rs:1477-1480`
  (`finish_search`); the label picks up `filter_note` at `main.rs:4106-4112`.
- The binding: `bind("commits", "/", "commits.search")`
  (`core/src/command.rs:463`).
- House rule (CLAUDE.md): a key is data and a command is a name; anything
  two clients need is a bug until it is in `core`. The *matcher* (what a
  query matches against a row) is data logic and belongs where the flatten
  rows live; the prompt is the shell's.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Everything | `./dev check` | exit 0 |

## Scope

**In scope**: `shell/src/views/files.rs`, `branches.rs`, `stashes.rs` (the
filter state + accessor discipline, modelled on commits);
`shell/src/main.rs` (`begin_search`/`finish_search` generalized;
`filter_note` label wiring for the three panes); `core/src/command.rs` (the
bindings and command registrations); `docs/` only where the keymap table
lists `/`.

**Out of scope**: fuzzy matching (substring, case-insensitive, exactly what
commits does today — improving the matcher is its own decision); searching
*inside the diff* (a different feature); the commits pane (already done);
section headers' counts while filtered beyond what `filter_note` gives.

## Git workflow

- Branch: `advisor/ui-048-list-search`
- Commits per step, e.g. `shell: the branches pane filters like the commits
  pane`
- No push, no PR, unless the operator instructed it.

## Steps

### Step 1: Read the exemplar end to end

Read `commits.rs`'s filter fields, `set_filter`, `filter_note`, the single
read accessor, and the `main.rs` search plumbing (`begin_search`,
`finish_search`, the live-edit routing, the label read). List the exact
member functions a pane must expose for the plumbing to drive it. That list
is the seam.

**Verify**: write the list in the commit message of Step 2 — it is the
review artifact.

### Step 2: Generalize the plumbing

In `main.rs`, make `begin_search`/`finish_search`/the live-edit routing
dispatch on the focused pane rather than hardcoding commits: a small match
(or a shared accessor on `Screen`) that reaches each view's
`set_filter`/`filter_note`. Register `files.search`, `branches.search`,
`stashes.search` in `core/src/command.rs` beside `commits.search`, each
bound to `/` in its own mode, each with a doc string in the house voice (the
help panel reads it). Remove the "not supported here" notice for panes that
now support it; keep it honest for any pane that genuinely cannot filter.

**Verify**: `cargo test -q -p gitten-core` → exit 0 (the keymap tests walk
bindings). `grep -n "is not supported here" shell/src/main.rs` → only
reachable for panes without a filter.

### Step 3: Filter state in the three views

Give files, branches and stashes the commits discipline, matched per pane:

- **branches**: match against the branch name (`main`, `origin/wip`);
  section labels (LOCAL/REMOTE) stay, but an emptied section drops its
  heading exactly as an empty section does today.
- **files**: match against the path (dir + name — the whole string the row
  shows); a filtered-out section drops as above.
- **stashes**: match against the stash message.

Rebuild the filtered index only on query change; keep every read behind the
one accessor; cursor stays on its row when it survives the filter and clamps
when it does not; any armed question disarms on filter change (the commits
tests state all four properties — copy their shape).

**Verify**: `cargo test -q -p gitten-shell` → exit 0, with per-pane tests
modelled on `a_query_filters_live_and_the_keyboard_stays_on_its_commit`.

### Step 4: The label carries the count

Wire `filter_note` into each pane's header the way commits' label does it
(`main.rs:4106-4112`) — shown over loaded, only while filtered.

**Verify**: `./dev check` → exit 0.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] `/` is bound in files, branches and stashes modes
      (`grep -n '"/"' core/src/command.rs` → four bindings)
- [ ] Each of the three views passes a live-filter test with the four
      commits properties (live, shown-not-loaded, cursor survival, disarm)
- [ ] Pane labels show `shown/loaded` while filtered
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/high-priority/README.md` row updated

## STOP conditions

- The live-edit routing in `main.rs` turns out to be structurally
  commits-only (e.g. it holds an `Entity<Commits>` rather than a pane name)
  in a way a match cannot generalize without reworking the prompt lifecycle
  — report the shape first.
- The commits filter's cursor rules depend on commit-specific anchors
  (`commits.rs:508` mentions anchors) that have no analogue in a sectioned
  list — report how sections and anchors should interact rather than
  guessing.
- Plan 045/046 executors are mid-flight in the same `main.rs` region and
  the operator has not sequenced you (see the README's dispatch waves).
