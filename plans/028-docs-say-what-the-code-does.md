# Plan 028: Make the docs say what the code does

> **Executor instructions**: Follow this plan step by step. Every item below
> was verified against the code at the planned-at commit — but code moves, so
> re-run each item's verification grep before editing, and if the code side
> has changed, update the doc to match the *live* code, not this plan's
> snapshot. If anything in the "STOP conditions" section occurs, stop and
> report. When done, update the status row for this plan in
> `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 87229df..HEAD -- README.md docs/ dev AGENTS.md .github/workflows/check.yml .gitignore shell/src/main.rs`
> Material drift in any of these → compare before editing.

## Status

- **Priority**: P2
- **Effort**: M (many S items, one pass)
- **Risk**: LOW
- **Depends on**: none. **Interacts with**: plan 023 also touches
  `.github/workflows/check.yml`'s header comment (item 6) — if 013 landed
  first, check whether the header is already accurate before editing.
- **Category**: docs
- **Planned at**: commit `87229df`, 2026-08-30

## Why this matters

This repo makes documentation binding: README's Contributing section says a
decision record *wins* until numbers beat it, and `docs/extending.md` turns
its `Host` listing into the extensibility test itself. Several of these
documents are now **actively wrong** — the most expensive kind of doc: a
contributor following decision 0024 would delete two CI jobs and re-add a
flag whose failure the workflow's own comments document; a user editing
`[keys]` for the desktop app gets silence; the shipped `--help` points at a
script that is now a shim. Every item below states both sides with locations;
the fix is to make the doc match the code (in one deliberate case, to record
a floor the code enforces).

Voice matters here: these docs explain *why* in full sentences and record
costs and dates. Match the surrounding prose. Minimal edits — amend the wrong
sentence, don't rewrite the section.

## Current state / the items

Each item: **doc says → code says → what to write.**

### 1. The desktop client does not read `[keys]` — three docs claim it does

- `README.md:126-127`: "every client reads the same `[keys]` table in
  `gitten.toml`, and the help panel is derived from it". `README.md:123`'s key
  table implies `q`/`?` work everywhere.
- `dev:32-33`: "Colour, font, `[diff]` and `[keys]` changes need none of
  this: gitten.toml reloads live in both clients" (also stale: there are
  three clients).
- `docs/clients.md:200-201`: "a colour, a differ, a wrap, a keybinding and
  `[view]` all apply" — contradicted nine lines later by its own
  `docs/clients.md:209-213`: "**`gitten-shell` does not read `[keys]`.**"
  (that later passage is correct and well-written — it is the model).
- Code: `shell/src/main.rs:691-707` — seven hardcoded `KeyBinding::new`
  calls; `grep -rn 'host.keys' shell/src` → no hits.
- **Write**: qualify each claim the way `clients.md:209` already does — the
  terminal reads `[keys]`; the window's bindings are hardcoded until it
  adopts `core::command` dispatch (that adoption is roadmap Phase A#2 —
  `docs/roadmap.md:34`). Fix the clients.md self-contradiction by qualifying
  line 200-201's "keybinding" mention, keeping 209-213 as is.
- Verify code side first: `grep -rn 'host.keys\|Keymap' shell/src | grep -v test` → empty.

### 2. Linux: "nobody has compiled it" — CI compiles and tests it on every PR

- `README.md:39-41`: "nobody has compiled it on Linux, so portability is a
  property of the source and not of a binary."
- `docs/architecture.md:271-274`: "nothing has been compiled or run there".
- Code: `.github/workflows/check.yml` — the `lint` job clippy-checks the
  whole workspace on ubuntu; `test-shell` (`check.yml:94-127`) runs
  `gitten-shell`'s headless GPUI tests on ubuntu and gates.
- **Write**: the honest current claim — every crate including the window
  compiles and its headless tests run on Linux in CI on every push; what has
  never existed is a Linux *binary anyone used*: no packaging, no windowed
  run, `./dev bundle` is macOS-only. Keep the "property of the source"
  phrasing if it survives — it is good — but it is now also a property CI
  enforces.

### 3. `docs/architecture.md` § "Not built yet" — three more stale bullets

- `:245-248`: "Any keyboard beyond scrolling, in the terminal … There is no
  `main`, no event loop and no keymap" → `tui/src/main.rs:69` is `fn main`
  with an event loop (`tui/src/term.rs:263`), and `tui/src/help.rs` derives
  the `?` panel from the keymap. Rewrite the bullet for what is *actually*
  still missing there, or delete it.
- `:249-252`: "Command dispatch and the mode stack. `Host` is where they
  belong. `s` and `w` are the only key bindings…" → `core/src/host.rs:64,69`
  ship `pub keys: Keymap, pub commands: Commands`; `host.rs:9-12` says
  dispatch landed. The remaining truth: the *desktop* doesn't consume them
  (see item 1). Rewrite to that.
- `:253-256`: "Configurable keybindings, and a settings panel" → `[keys]`
  parsing/validation/round-trip shipped in `app/src/config.rs`; the panel
  and the shell's consumption are what remain. Narrow the bullet.
- The Linux bullet is item 2. Check every remaining bullet against the code
  while you are in the section (the section's own header at `:239` promises
  "Listed so nobody reads an intention as a description").

### 4. `docs/extending.md` Host listing omits three real fields

- `docs/extending.md:13-22` lists 8 fields; `core/src/host.rs` has 11:
  `view: Scrolling` (`:51`), `mouse: Mousing` (`:57`), `themes: Themes`
  (`:85`). The same doc *uses* `host.themes.register(...)` at `:194-205`.
- **Write**: add the three lines to the listing with one-phrase comments in
  the listing's existing style. Verify field list first:
  `grep -n 'pub [a-z_]*:' core/src/host.rs`.

### 5. Decision records 0024/0025 describe a CI that no longer exists

- `docs/decisions/0024-ci-is-two-jobs.md`: title + "two jobs, both ubuntu";
  describes a `linux` job that was replaced (per 0025) and says exact cache
  hits "run Cargo offline" — directly reversed by
  `.github/workflows/check.yml:73-81`, which documents that offline was
  tried (`4f201af`) and broke main.
- `docs/decisions/0025-…md:25`: "Still two jobs."
- Code: four jobs — `test:33`, `lint:62`, `test-shell:94`, `audit:128`.
- **Write**: decision records are history — do NOT rewrite their bodies. Add
  an amendment block under each record's `**Status**` line (0024 currently
  says `**Status** accepted`), e.g.
  `**Status** amended 2026-08-30 — the workflow has grown to four jobs
  (test, lint, test-shell, audit; see check.yml's header) and the offline-on-
  exact-hit optimisation was reverted after 4f201af broke main; the two-job
  *shape* (correctness + lint, most of check.sh stays home) still stands.`
  Match each file's formatting.

### 6. `check.yml`'s own header says two jobs, in a file defining four

- `.github/workflows/check.yml:1-13`: "Two jobs, deliberately small:" then
  documents only `test` and `lint`.
- **Write**: extend the header's job list with one-line entries for
  `test-shell` and `audit` in the same style (each job already carries its
  full rationale inline — the header only needs the index). Skip if plan 023
  already fixed it.

### 7. The dev loop's new failure mode is undocumented

- `dev:26` ("`desktop` and `web` rebuild and relaunch on every save"),
  `AGENTS.md` § Building's dev-command table ("rebuild + relaunch on save"),
  and `README.md:90` all predate commit `87229df`, which changed the
  contract: `dev:101-106` now builds *first*, keeps the old window alive
  through the compile, and **a failed build leaves yesterday's binary
  running**.
- **Write**: one sentence in each of the three places: the window on screen
  during a red build is the *old* binary — check the terminal before
  believing the window. (The `dev` script's own comment at `:101-106`
  already says why; the user-facing texts don't say it happens.)
- Also `dev:32-33`'s "both clients" → "every client" (and see item 1's
  `[keys]` fix in the same sentence).

### 8. `docs/commit-graph.md` quotes geometry the code rejected for cause

- `docs/commit-graph.md:124`: "`DOT_R = 4.5`, `MERGE_R = 5.5`" → code:
  `shell/src/graph.rs:46-47` `DOT_R = 4.0`, `MERGE_R = 5.0`, with the
  comment at `:43-45` explaining 4.5 put a soft edge on the crisp lane line.
- `docs/commit-graph.md:130-133`: row-layout diagram labels "sha 90 / 26"
  in pixels → code: `shell/src/views/commits.rs:180-181` `SHA_CHARS = 12.0`,
  `WHO_CHARS = 3.0`, comment at `:176-179` explaining the pixel version overflowed.
- **Write**: update the numbers and, where the code comment carries the
  reason, echo it in a clause — this doc's readers trust its numbers.

### 9. The shipped `--help` points at the old script

- `shell/src/main.rs:106-108` (the `EXTRA` help block): "`./dev.sh <args>`
  rebuild and relaunch on every source change" → `dev.sh` is now a two-line
  shim that `exec`s `./dev desktop`; the entry point is `./dev`.
- **Write**: `./dev desktop <args>` and fold in item 7's caveat in one
  clause. This is a string constant in a code file — `cargo test -q -p
  gitten-shell` afterwards. Leave the other `dev.sh` mentions found by
  `grep -rn 'dev\.sh' --include='*.rs' .` alone if they are inside comments
  that are *about* the shim, but fix any that instruct a user
  (`shell/src/session.rs:6` and `shell/src/main.rs:791` are comments —
  update the command they name, keep their point).

### 10. Undocumented git version floor

- `git/src/lib.rs:219-222,232`: `--diff-merges=first-parent` is
  unconditional on the only `show` path and "needs git >= 2.31 (March
  2021)" per its own comment; on older git **every** commit view fails with
  git's unknown-option error. No README/docs mention any git version
  (`grep -rn '2\.31' README.md docs/` → nothing).
- **Write**: a "requires git ≥ 2.31" line in README's requirements/build
  area (near the build-from-source instructions), with the reason in half a
  sentence (first-parent merge diffs). A runtime probe/fallback was
  considered and deferred — record nothing about it in the README; it lives
  in this plan's maintenance notes.

### 11. `.gitignore`: `.claude/` is unignored

- `.gitignore` has four rule groups (`/target`, fixture patterns,
  `/gitten.toml`); `.claude/` exists (currently empty) and is unignored —
  one stray tool-written file away from `git status` noise or an `add -A`
  accident.
- **Write**: add `/.claude/` with a one-line comment in the file's existing
  commented style ("Local agent state, never the repository's business" —
  match the voice of the `gitten.toml` entry).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Shell still builds/tests (item 9) | `cargo test -q -p gitten-shell` | exit 0 |
| Nothing else broke | `./check.sh` | exit 0 |
| Lint/format (EXTRA is a Rust string) | `cargo fmt --check && cargo clippy -q --workspace --all-targets -- -D warnings` | exit 0 |
| YAML sane (item 6) | visual diff of `check.yml` — comment-only change | — |

## Scope

**In scope**: `README.md`, `docs/clients.md`, `docs/architecture.md`,
`docs/extending.md`, `docs/decisions/0024-ci-is-two-jobs.md`,
`docs/decisions/0025-formatting-and-lints-are-gated.md`,
`.github/workflows/check.yml` (header comment ONLY), `dev` (comments/help
text ONLY), `AGENTS.md` (§ Building only), `docs/commit-graph.md`,
`shell/src/main.rs` (the `EXTRA` string and the two named comments ONLY),
`shell/src/session.rs` (one comment), `.gitignore`, `plans/README.md`.

**Out of scope** (do NOT touch):
- Any executable code. If a doc fix seems to require a code change, the doc
  states the code's current truth and the gap goes to the report.
- `docs/interactive/index.html` — orphaned and stale; fixing it in place
  costs more than it returns. Leave it; the maintainer decides
  delete-or-regenerate (recorded in `plans/README.md`).
- `docs/roadmap.md`, `docs/measurements.md` — not part of these findings.
- CLAUDE.md (mirrors AGENTS.md — check: if it is a copy, apply the § Building
  sentence there too; if it diverges deliberately, leave it and note which).

## Git workflow

- Branch: `advisor/018-docs-say-what-the-code-does`
- One commit per item or per file, imperative and why-first — e.g.
  `clients.md: stop claiming the window reads [keys] eight lines before denying it`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

Work items 1–11 in order; each is independent. For each: run its
verification grep (given inline), make the edit, re-read the surrounding
paragraph aloud for voice.

**Verify after item 9**: `cargo test -q -p gitten-shell` → exit 0.
**Verify at the end**: `./check.sh; echo $?` → 0; fmt + clippy clean;
`git status --short` shows only in-scope files;
`grep -rn 'nobody has compiled' README.md docs/` → empty;
`grep -n 'Two jobs' .github/workflows/check.yml` → empty (or updated);
`grep -n '4\.5' docs/commit-graph.md` → empty.

## Test plan

Docs change; the tests are the greps above plus `cargo test -q -p
gitten-shell` for the string constant. No new tests.

## Done criteria

- [ ] All 11 items either edited or explicitly reported as "code drifted,
      wrote the live truth instead" with the new location
- [ ] The end-of-steps grep battery passes
- [ ] `./check.sh` exits 0; fmt and clippy clean
- [ ] No executable-code changes (`git diff` shows only strings/comments in
      the two `.rs` files)
- [ ] `plans/README.md` status row updated

## STOP conditions

- A doc claim you are about to correct turns out to be TRUE at HEAD (the
  code caught up) — skip the item and say so.
- Fixing an item seems to require choosing between two plausible truths
  (e.g. whether the shell *should* read `[keys]` soon changes how item 1 is
  phrased) — phrase it as `clients.md:209` already does (current fact + the
  porting note) and flag the tension in your report rather than deciding
  roadmap in a docs pass.

## Maintenance notes

- Deferred from item 10: a `git --version` probe that drops
  `--diff-merges=first-parent` below 2.31 (merges render empty on ancient
  git, everything else works). S-effort, `git/src/lib.rs`, worth doing if a
  real user hits the floor.
- The doc-drift generator here is structural: `architecture.md` § "Not built
  yet" and the decision records have no owner-on-change. A cheap ratchet
  (suggested, not planned): a CI grep test pinning the two or three most
  load-bearing claims (e.g. `check.yml` job count vs 0024's amendment). The
  maintainer may prefer discipline over machinery.
- Reviewers: check voice. These docs are written in a specific register;
  a bolted-on sentence in changelog-speak is a regression even when true.
