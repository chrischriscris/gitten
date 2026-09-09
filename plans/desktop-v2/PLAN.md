# Desktop v2 (guide-v2) — build plan

Source of truth: `artifacts/guide-v2/IMPLEMENTATION.md` + runnable reference in
`artifacts/guide-v2/` (fixtures only — visual/interaction reference, never port
its handlers or copy). Philosophy/boundaries: `AGENTS.md`, `docs/architecture.md`,
`docs/clients.md`, `docs/theming.md`.

## Verdict (decided)

Retrofit in place via strangler, not a second plate. `core/` keeps zero UI
knowledge; grouping/selection seams go below the client where tui/web/extensions
can reuse them. All mouse controls resolve through existing named commands
(`core::command`) — no parallel write path. Mouse-first now; keyboard rebinding
later is free because a key is data and a command is a name.

## Target composition (from IMPLEMENTATION.md)

- Toolbar 53px: platform traffic lights, branch control (`name` + `from <base>`),
  Commands (`commands.palette`, Cmd/Ctrl-K), Push + outgoing `ahead` count.
- Sidebar 255px (280px >1550px): repo identity, Changes/History nav,
  Branches/Stashes controls, `/` filter, directory-grouped files with stage
  checkbox (empty / `−` mixed + `n/m` hunk fraction / check).
- Workspace header 76px: destination title, real file/change counts,
  working-copy status.
- Center diff: breadcrumb, Unified/Split toggle (selection-preserving), file
  status + totals, per-hunk Stage buttons; existing pipeline untouched
  (registry, `Prepared`, `align`, blob-OID cache, `uniform_list`, fixed gutter,
  capture-phase gesture lock).
- Inspector 266px fixed right (295px >1550px): Commit heading, staged-file
  summary (path + staged/total hunks), Summary + optional Description drafts
  (survive nav/refresh/failure, cleared on success only), staged-hunk count,
  Commit gated on staged content + non-whitespace summary, confirmation dialog.
- Status bar 29px: fetch/push recency, remote, staging count, hints + version.

## New / changed files (ownership — one writer at a time in this worktree)

| Piece | Files | Lane |
|---|---|---|
| Dir-group seam + hunk-fraction query (pure, tested) | `core/src/groups.rs` (new), `core/src/lib.rs`, `app/src/act.rs` (toggle fn) | core-seam |
| Guide theme | `core/src/theme.rs` (`Theme::guide()`), `core/src/host.rs` (register) | theme |
| Toolbar + status bar | `shell/src/main.rs` (TITLE_H→53, lights, branch control, push), `shell/src/chrome.rs` (STATUS_H→29, segments) | chrome |
| Sidebar | `shell/src/views/sidebar.rs` (new), `shell/src/views/files.rs` (grouped flatten) | sidebar |
| Center dock | `shell/src/views/diff.rs` (header hooks, single-file projection, selection-preserving toggle, per-hunk buttons), `shell/src/views/mod.rs` | diff |
| Inspector | `shell/src/views/inspector.rs` (new), `app/src/act.rs` (draft store), `shell/src/input.rs` (multiline description) | inspector |
| Workspace shell | `shell/src/views/workspace.rs` (new), `shell/src/main.rs` (assembly), old stack removal last | workspace |

## Order

1. **Phase 1 (now):** core dir-group seam + unit tests; `Theme::guide()` +
   `contrast` example check + registration. Verifiable headless.
   (`cargo test -p gitten-core`, contrast run.)
2. **Phase 2 (landed):** workspace shell behind named-command toggle +
   sidebar grouped flatten + center header + single-file projection.
   Toggle seam (no core change, no keybinding yet — the palette in Phase 3
   is the human door; tests drive the names directly):
   `workspace.changes` (enter, focuses files), `workspace.history` (back to
   the full stack = History destination for now), `workspace.preview`
   (re-aim center at the files cursor; sidebar clicks dispatch it after
   `select_row`, the command tail + refresh wave cover the rest).
   Center keyboard/wheel work with spot==Main via an explicit
   `run_command_from` door + capture-phase wheel branch; layout toggle
   calls `set_layout` directly (view-local presentation state, selection
   carry is Phase 4); conflicts skip the preview (own markers
   presentation); sidebar wheel follow + full gesture audit are Phase 4.
   Also absorbed: two `app` theme-list test expectations updated for the
   Phase 1 `guide` registration ("shipped seven"→eight).
3. **Phase 3:** inspector (drafts, gating, confirmation, real commit) + toolbar
   branch/push/commands + status segments.
4. **Phase 4:** selection-preserving Unified/Split toggle, per-hunk buttons,
   gesture audit, responsive rules, contrast resolution on all 11 surfaces.
5. **Phase 5:** delete old stack code, full acceptance (IMPLEMENTATION.md list),
   `./dev check`, `./dev dump` visual.

## Invariants (every phase)

- No per-frame regroup / diff prep / unbounded shaping; flatten-once,
  OID-keyed cache, `uniform_list`.
- `ahead: Option<u32>` — `None` renders `—`, never `0`.
- CSS hexes are targets; resolve via `readable()` (`min_contrast` 3.5,
  `min_furniture` 3.0); every theme change through `reload`/`patch` so
  `sync_widgets` + `Theme::sync_base` run.
- Refusals verbatim at the operation site (hooks, non-FF push, conflicts);
  never success copy over failure. Destructive safeguards untouched.
- Never `window.viewport_size()` from a view; probe/measure pane bounds.
- Do not launch a client unasked; verify with `./dev check` + `./dev dump`.

## Recon outputs (this plan's evidence)

Scout maps from the parallel recon wave are summarized here; per-lane details
with file:line refs live in the subagent artifacts for this session:

- toolbar-status: TITLE_H/STATUS_H constants, `commands.palette` registration,
  branch control from loaded branches state, push via `repo.push` + `ahead`,
  `last_fetch/last_push` shell timestamps, staging count from prepared files state.
- sidebar-files: `group_by_dir` + `staged_fraction(path)` seams, `Entry` gains
  dir headings, checkbox vs filename hit targets, `stage_remainder_or_unstage`
  in `app::act`, 255px vs `sidebar_share` decision.
- center-diff: parent assembly + center header, one-element `replace_prepared`
  projection, anchor + re-resolve toggle, `hunk_at` button resolution, gesture
  scope to `list_bounds()`.
- inspector-commit: draft store outside prompt slot, multiline Description,
  gating + confirmation through `files.commit`, staged-only commit, draft kept
  on failure.
- theme-workspace: `Theme::guide()` port rule, px↔share conversion, strangler
  order, single-`Font` vs dual-face open question.

## Phase 3 status (stabilize-and-commit run; parent timed out mid-wave)

Commit `desktop-v2 phase3-partial` compiles green: core 498, app 155,
 shell 397; `cargo fmt --check` clean for the shell package.

DONE (in the tree, tested where noted):

- `workspace.changes/history/preview` registered in `Commands::builtin`
  (`core/src/command.rs`) AND shell dispatch; new core test
  `workspace_commands_are_registered_and_bindable` proves the `known()`
  gate passes — a `[keys]` `"W" = "workspace.changes"` entry now loads.
- `TITLE_H` 44→53, `STATUS_H` 40→29 (`chrome.rs`); `status_bar` +
  `hints_budget` take `leading` segments; statusbar-height test updated
  40→29. Old-stack call sites pass `&[]` (TODO phase3-status).
- `files::prepare` takes hunk-fraction `counts`; `side_hunk_counts`
  (`app/src/acquire.rs`, repo-arg cleanup) is called on the refresh path
  (`main.rs`); `flatten` renders from it. Needs visual QA.
- `act::stage_remainder_or_unstage` (new, staged) + shell caller +
  unit test — the sidebar checkbox verb (partial→stage remainder,
  full→unstage). Needs click-level QA.
- `CommitDraft{summary, description}` + `has_message`/`message` gate
  helpers; per-repo `drafts` map + `commit_confirm` flag +
  `last_fetch`/`last_push` timestamps exist on `DevShell` and are
  constructed — all currently UNREAD (dead-code warnings).
- `commands.palette` dispatch arm (reuses `toggle_help`: one panel, two
  names) + `cmd-k` binding; `CommitStaged` menu action + `cmd-enter`
  binding routing to the `workspace.commit` named door.
- `input.rs` multiline groundwork (`set_multiline`, `set_text`,
  `insert_newline` present, unwired per warnings).

STUBBED (present but inert — the next worker's list):

- `open_commit_confirm` / `confirm_commit` / `cancel_commit_confirm`
  (`main.rs`): notice stubs ("commit confirmation arrives with the
  inspector"). No confirm dialog, no real commit from the inspector;
  the old `files.commit` prompt path is untouched and still works.
- `views/inspector.rs` (new, ~235 lines): `render_inspector`,
  `InspectorDeps`, `StagedFile` exist but are referenced NOWHERE — the
  inspector is not in any render path. Wiring it into `workspace.rs`
  (266px slot) is the core of the resume run, incl. drafts↔fields,
  staged summary from `counts`, gating via `has_message` + staged total.
- Status-bar leading segments (sync state, remote, staging count):
  computed nowhere; `last_fetch`/`last_push` never stamped on job
  finish; hints budget costs `[]`.
- Toolbar: no branch control, no Push button + `ahead` count, no
  Commands button (menu adapters + keys exist; the 53px strip's
  controls do not).
- `commit_confirm` dialog state, draft-field editing wiring,
  `staged_summary`/`staged_hunks` methods: written, never called.

Resume order for the next worker: wire inspector into workspace render
→ drafts + staged summary + gating → confirm dialog through
`act::commit_message` (staged-only, keep draft on refusal) → status
segments + job-finish timestamps → toolbar controls → visual QA via
`./dev dump`. Do NOT launch any client.

## Phase 3-resume-A (landed): inspector wired

Commit `desktop-v2 phase3-resume-A: inspector wired`: shell 399 (incl. 2
new `CommitDraft` gate/message tests), core 498, app 155; `cargo check`
zero warnings; `cargo fmt --check` + `cargo clippy --workspace
--all-targets -- -D warnings` clean.

DONE:

- `views/inspector.rs` renders in the workspace's 266px slot (heading,
  staged-file summary from `files::staged_summary`, Summary single-line +
  Description multiline `Input` entities, staged-hunk count, Commit
  button, gate reason line under the button when disabled).
- `ensure_inspector_fields` builds both fields once on first composition
  and refills per repository key; `Edited` subscriptions mirror every
  keystroke into `drafts`, with `sync_fields_to_draft` as the transition
  backstop (dialog open/submit, leave, refill). `set_text` refill emits
  nothing, so no feedback loop.
- Gate: staged-hunk total > 0 AND draft `has_message`; disabled button
  stays clickable and says why (button note + notice on the keyboard
  door). Confirm dialog (`modal::centered`) shows branch (`head_info`),
  draft message, files/hunks; confirm re-gates and submits through
  `act::commit_message` (staged-only, unstaged retained). Draft clears
  only on the `commit` job's clean finish, keyed by `pending_commit_key`
  (exact repo, never current-by-accident); refusal keeps the draft with
  git's verbatim error in the band and closes the dialog.
- Keyboard: field focus bypasses the pane keymap (typing never fires
  commands); Esc blurs to files; plain Enter advances Summary →
  Description; Alt+Enter breaks the Description line (`input.newline`
  convention); Cmd+Enter commits via the existing global door. Esc over
  the dialog cancels with focus restored to files.
- Remaining `STUB(phase3-resume)` markers are exactly the resume-B set:
  `last_fetch`/`last_push` stamps + `files::counts()` for status
  segments. Toolbar controls untouched.

STILL STUBBED (resume-B): status-bar leading segments + job-finish
timestamps; toolbar branch control, Push + `ahead`, Commands button;
visual QA.

## Phase 3-resume-B (landed): toolbar + status

Commit `desktop-v2 phase3-resume-B: toolbar + status`: shell 402 (incl. 3
new spelling tests + pump-test stamp asserts), core 498, app 155;
`cargo check` zero warnings; `cargo fmt --check` + `cargo clippy
--workspace --all-targets -- -D warnings` clean.

DONE:

- Job-finish stamps in `drain_jobs` (Ok arm only): `pull`/`fetch*` →
  `last_fetch`, `push *` → `last_push`. A refusal stamps nothing — the
  previous recency stands beside the band's verbatim error.
- Status bar leading segments via memoized `status_leading` (key: stamps,
  staged total, remote, branch label; frames otherwise clone three
  refcounts): single sync sentence (newest stamp wins; `Never fetched`
  when neither ran), remote from loaded upstream or em-dash (no per-frame
  git call — the single-remote fallback stays a recon note), staging
  count from `files::counts()` (consuming its STUB marker). Both
  `TODO(phase3-status)` markers consumed; `hints_budget` shrinks by the
  segments it already knew how to cost.
- Toolbar: branch chip is now `branch-control` (click → `branches.focus`)
  with dim `from <base>` beside the name; `commands-button` (→ same
  `commands.palette` as cmd-k + menu adapter); `push-button` (→ same
  `repo.push`; `None` → `Push —`, `Some(0)` → inert dim `Published`,
  else `Push N`; detached stays live so its refusal surfaces verbatim).
  All ids unique with `debug_selector`s; no new dropdowns (no
  deferred/occlude needed — palette/notice reuse existing dialog paths).
- Zero `STUB(phase3-resume)` / `TODO(phase3-status)` markers remain.

STILL STUBBED (Phase 4): selection-preserving Unified/Split toggle,
per-hunk Stage buttons, sidebar wheel follow + full gesture audit,
responsive rules, contrast resolution on all 11 surfaces, system-font
vs mono decision. Visual QA so far is headless (code re-read for ids,
focus paths, memo discipline); `./dev dump` is TUI-only and the desktop
window was not launched per project rules.
