# 004 — W10b: diff and log options, the file tree, screen modes, the copy family

Packet **W10**, second half, from
[001 — TUI lazygit parity swarm](001-tui-lazygit-parity-swarm.md). One worker, four
slices. Read 001 §"Architectural requirements", §"Swarm execution contract" and
§"Verification gates" first; they bind this document.

Fourteen ledger rows, none of which spawn a process, open a window or write to a
repository. That is the whole reason this half exists separately: it is the lowest-
risk lane in the campaign and it can run concurrently with everything else.
[003](003-w10a-external-tools-and-custom-commands.md) owns anything that executes.

## Objective

The knobs a reader of diffs and history actually reaches for, and cannot reach for
today: context size, whitespace, rename detection, what the diff is *against*, what
the log includes, how the file list is shaped, how much of the screen a pane gets,
and copying the thing under the cursor rather than the row it is drawn on.

Each is small. What they share is that every one of them is a *pure function of a
setting plus already-acquired data*, so the correctness question is always the same
one: which cache does this invalidate, and does the invalidation actually happen.
Answer it per row, in the doc comment, with the cache named.

## Ledger rows

| ID | Row | Importance | Slice |
|---|---|---|---|
| LG-103 | Increase/decrease diff context (`}` / `{`) | minor | 1 |
| LG-104 | Toggle whitespace changes in diff (`ctrl+w`) | daily | 1 |
| LG-106 | Rename similarity threshold | minor | 1 |
| LG-107 | Diff against selected ref, enter a ref, reverse (`W`, `ctrl+e`) | daily | 1 |
| LG-105 | Cycle diff renderers — confirm or record as an intentional difference | minor | 1 |
| LG-108 | Filter the commit log by path (`ctrl+s`) | daily | 2 |
| LG-109 | Sort branch order (`s`) | minor | 2 |
| LG-110 | Log options: sort, hide/show graph (`ctrl+l`) | minor | 2 |
| LG-111 | Show/cycle all-branch logs | minor | 2 |
| LG-113 | File tree view: flat/tree, collapse/expand all | minor | 3 |
| LG-013 | Filter files by status (staged/unstaged) | minor | 3 |
| LG-012 | Filter a menu or prompt by typing | minor | 3 |
| LG-102 | Screen modes: normal/half/full (`+` / `_`) | minor | 4 |
| LG-089 | Copy path / branch / tag name | minor | 4 |
| LG-090 | Copy abbreviated commit hash | minor | 4 |
| LG-091 | Copy a commit attribute | minor | 4 |

Do not edit `tui-parity-ledger.md`; the coordinator owns it.

## Base, worktree, ownership

```sh
# from /Users/chus/Projects/gitten, once:
git worktree add .worktrees/tui-parity-w10b -b feat/tui-parity-w10b 529ec80
```

- **Your only working directory** is `.worktrees/tui-parity-w10b`. Never write to
  `/Users/chus/Projects/gitten`, `/Users/chus/Projects/gitten-ux-polish`, or another
  `.worktrees/*`. Never push. Commit locally, one commit per working increment.
- **Owned outright:** `core/src/tree.rs` (new), `core/src/copy.rs` (new),
  `tui/src/files.rs`, `tui/src/diff.rs`, `tui/src/commits.rs`,
  `tui/src/branches.rs`, `tui/src/rows.rs`, and your tests.
- **Shared, additive only**, inside `// --- w10b begin` / `// --- w10b end` blocks:
  `core/src/lib.rs`, `core/src/command.rs`, `core/src/differ.rs` (settings surface
  only), `core/src/host.rs`, `app/src/{config,acquire}.rs`, `git/src/lib.rs`
  (acquisition arguments only), `tui/src/main.rs`, `tui/src/panes.rs` (the `Layout`
  registry only — W9 adds `canonical_rank` entries in its own block).
- **Never touch** `tui/src/stashes.rs` (W7), `tui/src/term.rs` or `app/src/tools.rs`
  (W10a), or anything under `shell/`.

## What already exists — do not rebuild it

- **The diff cache is keyed on the pair's blob OIDs plus every setting that reaches
  the answer** — resolved algorithm, whitespace relation, context, move floor,
  indent heuristic — in `core::differ`, bounded, shared through one `Arc`. A live
  context or whitespace change therefore *already* produces a different key. Your job
  is to make sure the setting reaches the key and that the prepared rows downstream
  are rebuilt, not to add a second cache or to clear this one.
- Three distinct layers, and knowing which one a row touches is most of the work:
  **acquisition** (`git diff --raw` + `cat-file`, so the revspec and `-M` live here),
  **preparation** (`core::differ` + `core::prepared`: context, whitespace, the
  algorithm), **presentation** (`core::rows` and the layout/wrap registries). Context
  does not re-acquire. A ref change does.
- Registries already exist and already feed their own pickers: the layout registry
  (shell-side implementations, names on `Host`), the wrap registry (implementations
  in `core`, column count from the frontend), and the keymap. Anything you add as a
  registry entry appears in the pickers and the help panel for free; anything you add
  as a flag beside one does not. `off` being an entry rather than a flag is the
  precedent.
- `core::search` is the existing text-match model (`/`, W2's `n`/`N`). The menu
  filter (LG-012) is that model over a menu's rows, not a second matcher.
- `Terminal::copy` is OSC 52, chosen because a terminal is often not on the machine
  the clipboard is on. The copy family adds payloads, never a second transport.
- `tui/src/panes.rs` holds geometry as data (`Rect` per pane, cached by the caller)
  behind a `Layout` **trait with a built-in**, so a compiled-in extension can replace
  the geometry without touching registry or dispatch. Screen modes are three
  registered layouts, which is why LG-102 is four lines of policy and not a rewrite.
- `--topo-order` is on every history traversal and lane assignment assumes it.
  Read the AGENTS.md paragraph about it before slice 2; it narrows git/git from 417
  to 280 lanes and *widens* cmux from 19 to 73, so it is correctness, not compactness.

## Slice 1 — diff options

Four settings and one decision.

- **Context (`}` / `{`)** and **whitespace (`ctrl+w`)** already exist as
  `[diff] context` and the differ's whitespace relation; what is missing is a live
  command and a bound key. Live means: change the setting, re-prepare, redraw, and
  keep the cursor on the same *line of the file* — not the same row index, because
  changing context changes how many rows there are. That is the visible bug if you
  get it wrong, and it needs a test that asserts the selected line after a context
  change.
- **Rename similarity (LG-106)** is an acquisition argument (`git diff --raw -M<n>`),
  so it invalidates the pair list, which invalidates everything downstream. Bound it
  sanely (git's own 0–100) and refuse the rest with a sentence.
- **Diff against a ref (LG-107)** is a revspec change: `Repo::pairs(revspec)` already
  takes one, and the CLI already accepts revspec comparison — this row is the *UI*
  for what acquisition can already do. Three commands: diff against the selected ref
  row, enter a revspec at the prompt, and reverse the comparison. Reverse is
  implemented at the revspec (`B..A`), not by swapping sides after the fact, because
  rename detection and the synthesized untracked side are not symmetric.
  **A revspec cannot smuggle an option to git** — there is already a test named that
  in `git/src/lib.rs`, and a prompt is exactly how someone would try.
  While a comparison stands, the client must *say* what is being compared. A diff
  view that silently shows something other than the working tree is the worst failure
  mode in this slice.
- **LG-105 is a decision.** The ledger flags counting the layout registry as the
  equivalent of upstream's renderer cycling as a judgment call, to confirm here.
  Write the case, recommend, and record it as an intentional difference — with the
  key difference (`s`) named — for the coordinator to sign off. No code unless the
  answer is "something is genuinely missing".

**Acceptance.** Context up and down changes hunk boundaries and keeps the selected
line; whitespace-ignoring changes the changed-line count on a fixture with only
indentation changes and leaves the *displayed text* the original bytes (the
normalisation is an equivalence relation, not an edit — AGENTS.md); `-M` changes a
rename fixture's pair list; a ref comparison against `HEAD~2` shows what
`./dev tui diff . HEAD~2..HEAD` shows, and the reverse shows its mirror; a revspec
beginning `--` is refused. Frames at 120x40 and 80x24 prove the "comparing against
X" label is legible and not clipped.

## Slice 2 — log options

- **Path filter (LG-108).** A pathspec reaches `log`/`log_stream`. Two things must
  survive it: `--topo-order` stays, and lane assignment keeps reading the parents git
  *reports* — history simplification rewrites them, and a graph drawn from
  unsimplified parents against a simplified list is the silent wrong drawing. Test
  the lanes, not only the row count.
- **All-branch logs (LG-111).** `--all` (and the cycle upstream offers). The 12-lane
  cap already exists for exactly this; measure git/git with `--all` and record the
  numbers rather than guessing.
- **Sort (LG-109 branches, LG-110 log).** Branch sort is `for-each-ref --sort`, which
  is free and safe. Log sort is not: an ordering that does not guarantee children
  before parents breaks lane assignment. Any ordering you offer must be validated on
  the git/git fixture — lane count, and a dump frame you actually look at. An
  ordering that fails validation is **refused with a named reason**, which is W0's
  contract and better than a wrong graph.
- **Hide/show the graph (LG-110).** A `[view]` setting plus a live key; the commit
  row builder omits the gutter. This is a product change (gitten always draws the
  graph), so it earns its place with the number: how much width comes back on
  git/git, where 280 lanes collapse into a 12-lane cap today.

**Acceptance.** A path filter on a real fixture lists exactly the commits touching
that path, with a correct graph; `--all` includes a commit reachable only from
another branch; branch sort reorders by committer date and by name; the graph
toggle changes the row's width and nothing else. Record the lane counts and the
before/after row/cell counts from `./dev dump` in your report — this slice is the
one with performance surface, and `docs/measurements.md` is where the coordinator
files it.

## Slice 3 — the file list

- **The tree (LG-113)** is a shared model: `core::tree` builds a directory tree from
  byte paths, with collapse/expand and a flat/tree toggle, and it must serve the
  files pane now and W8's commit-files pane later without a second implementation.
  Paths are bytes; a directory whose name is not UTF-8 renders lossily and still
  addresses the real path. A tree row is a **row** — more rows, never a taller one —
  because `uniform_list` and the terminal's row model both require uniform height.
- **Status filter (LG-013)** is a predicate over the files model's sections
  (staged / unstaged / untracked / conflicted, which porcelain-v2 already separates).
  The filter is *visible* when it is on: a filtered list that looks like an empty
  repository is the bug.
- **Menu filter (LG-012)** is `core::search` over a menu's rows, with a no-results
  state that says so.

**Acceptance.** A tree over a fixture with nested directories, a path with a space,
and a non-UTF-8 directory name; collapse-all/expand-all; the selection surviving a
flat↔tree toggle (same *file* selected, not the same index); a status filter that
hides a section and says it is filtering; a menu filter that matches, that
non-matches, and that clears. Frames at 80x24 for the deep-nesting case.

## Slice 4 — screen modes and the copy family

- **Screen modes (LG-102)** are three entries in the `Layout` registry — normal, half
  (the focused pane takes more of the body), full (it takes all of it) — with `+`/`_`
  cycling. The narrow layout below `WIDE_AT` already gives the body to the focused
  pane, so "full" must compose with it rather than fight it: at 80 columns the modes
  are the same frame, and that is correct.
- **The copy family (LG-089, LG-090, LG-091)** is one shared payload model,
  `core::copy`: given a target (a file row, a branch, a remote branch, a tag, a
  commit, a reflog entry) and an attribute (path, absolute path, name, short sha,
  full sha, subject, author, message, diff), produce a label and the bytes. Shared
  because the window needs the same payloads and must not write them again. The
  terminal's transport stays OSC 52.
  `y` copies the whole row as text today; keep it, and add the attribute commands
  beside it — a key change is a product decision, not a quiet edit. LG-091's menu is
  a menu of the attribute list, which means the menu is a pure function of the
  registry and gets its filter from slice 3 for free.

**Acceptance.** Each mode's frame at 120x40 (dump, and look at it — a passing state
test does not prove legible layout); each copy payload asserted as bytes, including
a path with a space, a non-UTF-8 path (the real bytes, not the lossy display), and a
commit whose subject is Latin-1; the OSC 52 sequence asserted once, since
`base64_is_the_one_in_the_rfc` already covers the encoding.

## Hazards

- **Every row here is a cache question.** Name the cache in the doc comment. A
  setting that changes the answer and not the key is a stale diff that looks right.
- Do not renumber or repurpose shipped keys to match upstream. `s`, `y`, `o`, `D`,
  `g`/`G`, `]`/`[` already mean something; the ledger records the differences
  deliberately. A remap is a coordinator decision.
- Keep the render path clean: derived state is computed at load and cached, never per
  frame. A tree, a filter and a layout mode are all load-time work.
- The frame timings in a debug build are meaningless. Use `--release` for any number
  you report, and never compare across build profiles.
- Nothing in this packet adds a dependency to `core/`, and nothing in it needs one.

## Gates

```sh
cargo fmt --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p gitten-core -p gitten-app -p gitten-git -p gitten-tui
cargo test --locked -p gitten-core -p gitten-app -p gitten-git -p gitten-tui tui_parity_
COLS=120 ROWS=40 ./dev dump commits .
COLS=80  ROWS=24 ./dev dump commits .
COLS=120 ROWS=40 ./dev dump diff --fixtures
COLS=120 ROWS=40 LAYOUT=split ./dev dump diff --fixtures
./dev --release dump commits ~/Projects/git 600   # slice 2's numbers
```

`./dev check` is the coordinator's to schedule. Never launch `./dev tui` or
`./dev desktop`.

## Proof protocol

1. Verbatim `git log --oneline 529ec80..HEAD` and `git diff --stat 529ec80..HEAD`.
2. Every claimed commit resolves under `git cat-file -e`.
3. Every claimed test named from captured test output.
4. Gate tails with exit statuses, not summaries.
5. Dump frames pasted for the layout claims — this lane's whole product is what the
   screen looks like, so a claim about a frame without the frame is not evidence.
6. A failing gate is reported as failing. A smaller true report beats a larger false
   one.

## Out of scope

Anything that runs a program (003 owns it, including the difftool row and the
browser rows). W7's stash/tag/reflog. W8's patch builder. The desktop window (005).
The intentional key differences the ledger already records — read them, do not
"fix" them.

## Dispatch text

> You are the W10b implementation lane for gitten. Read
> `plans/004-w10b-view-conveniences-and-copy.md` in full, then
> `plans/001-tui-lazygit-parity-swarm.md` and `AGENTS.md`. Work only in
> `.worktrees/tui-parity-w10b` on `feat/tui-parity-w10b`, based on 529ec80. Take the
> slices in order and commit after each working increment; for every row, name the
> cache it invalidates in the doc comment. Deliver working code, named `tui_parity_`
> tests and pasted dump frames, not stubs and not a new plan. Never push, never
> touch another worktree, never launch a client. Finish with the plan's proof
> protocol.
