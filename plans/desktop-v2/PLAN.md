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

## Phase 5 status (landed): workspace default + acceptance

Commit `desktop-v2 phase5: workspace default + old-stack removal`:
shell 410 (incl. 2 new), core 498, app 155; `cargo fmt --check` +
`cargo clippy --workspace --all-targets -- -D warnings` clean.

DONE:

- Workspace is the launch destination: `Workspace::default()` is
  `enabled` on Changes; the startup path runs `enter_workspace` on
  frame one (center built, files focused, preview scheduled; fixture
  launches early-out cleanly, skeleton waves re-aim on landing).
- Real robustness fix found by the flip: the `view.*`/`diff.*`
  keyboard door routed on the `enabled` flag alone, swallowing keys
  for a center that did not exist yet. It now requires
  `center.is_some()` too.
- 7 stack-pinning tests now lower the workspace first
  (`workspace.history`), documenting that the old composition is the
  History destination; one also re-focuses stashes after lowering
  moved focus to commits.
- New coverage: `a_refused_commit_keeps_its_draft_and_a_clean_one_spends_it`
  (production pump both ways: refusal keeps words + closes dialog +
  verbatim hook error; clean finish spends exactly that repo's draft)
  and `the_workspace_is_the_launch_destination` (default pin).

REMOVED vs KEPT (deliberate, each with its reason):

- Removed: nothing structural. The stacked-panes assembly
  (STACK_TOP/STACK_FOOT, `sidebar` + `main_region` construction,
  `pane_header`, divider drag) still renders the History destination
  (`workspace.history` lowers onto it), so it is live code, not dead
  code — deleting it deletes History, which the interaction contract
  requires as a separate destination. Full deletion awaits the
  History timeline moving into the workspace (defined follow-up).
- Kept: `files.commit` prompt path (still the bound `c` key + help +
  tests; removing it removes keyboard commit), `panes.rs` registry
  (untouched), all destructive safeguards (untouched, spot-checked by
  existing tests: discard two-press, reset guards, amend/refusal
  paths).
- Known waste, not fixed: `sidebar` + `main_region` subtrees are built
  every frame even while the workspace is up (used only by the History
  branch). Same order of cost as normal render construction and
  dwarfed by virtualized-row work; lazily branching them is a
  follow-up, not a blocker.

## Old-stack deletion (landed on top of 7bfab6c)

Precondition met by the user's History-timeline commit: `workspace.history`
now renders the in-workspace timeline, so the numbered-stack composition
had no production reader — the only `enabled = false` write was the
`#[cfg(test)] leave_workspace`. Net: ~2880 lines removed, ~240 added.
shell 393 · app 155 · core 498 green; fmt + workspace clippy `-D warnings`
clean.

- Removed: `leave_workspace` + the `false` render branch (old sidebar +
  main_region + divider), STACK_TOP/STACK_FOOT, old sidebar-stack
  builders, divider drag, `chrome::pane_header(_with)`, the status-bar
  hint projection (`hints`, `hints_budget`, `version`, badge consts,
  `hint_air`) + its 4 projection tests — the workspace bar was already
  designed without them (leading segments + Commands door, per the mock).
  Stack-only tests migrated to `workspace.history` or deleted with reason.
- Scoped to `#[cfg(test)]` (branches.rs precedent): commits/stashes
  `filter_note`, `active_view_name` — tests still pin real logic through
  them; the workspace reads only the files pane's note.
- Kept: all view panes (workspace reuses them), `files.commit` prompt
  behind `c`, `panes.rs`, safeguards, registry-level hint test, `?` help.
- Muscle checked, not cut: the pane-trait impls the workspace still uses
  (`any`, `list_bounds`, scroll verbs) are intact — `cargo check` passes
  and the History timeline test renders timeline beside commit diff.

ACCEPTANCE (headless-verifiable subset; window-only items need eyes):

- Startup default / selection→diff-only / partial+full staging +
  mixed checkbox / unstage / staged-only commit w/ unstaged retained /
  Changes↔History nav + draft survival / push counts + `—` for
  unknowable `ahead` / hook-failure + refusal verbatim at the
  operation site / empty status (`0 changed` + grouped empty tree) /
  detached-HEAD + gone-upstream refusal paths: covered by existing +
  new tests, all green.
- Long paths / large diffs / resize breakpoints / light+dark visual
  contrast / GPUI rendering: NOT verifiable headless (`./dev dump`
  is TUI-only). The 1550px + 1150px rules are pure functions with
  tests; what they look like needs the window.
- `./check.sh`: everything green EXCEPT `diffcheck(., HEAD~4..HEAD)`
  — our patience vs `git --patience` diverges +22/1767 changed lines
  on `shell/src/views/sidebar.rs` (a new file of near-identical
  `div()` builder chains, the adversarial shape for anchor choice).
  Reproduces on clean 1759521 (stashed), and `core/src/differ.rs` is
  untouched across the whole branch: pre-existing algorithm behavior,
  not a v2 regression. Left for a dedicated differ pass — touching
  the differ at this hour, for all clients, is the wrong call.

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

## Phase 4A status (stabilize run on top of 24c4a78)

DONE:

- Selection-preserving toggle: `Selection::head()` accessor
  (`core/src/select.rs`) + `CarriedCaret`/`CarriedSelection` snapshot and
  `restore_selection` in `views/diff.rs`, with round-trip tests
  (`a_selection_survives_a_reflow_and_crosses_a_layout_change`,
  `a_selection_survives_a_layout_round_trip`).
- Per-hunk buttons: `hunk_for_row` / `hunk_content` / `loaded_hunks`
  (`views/diff.rs`) + shared `submit_hunk_patch` tail and
  `workspace_stage_hunk` (`main.rs`), with the
  `button_row_and_keyboard_name_the_same_hunk` test proving button and
  keypress address the same hunk.
- Gesture audit / sidebar wheel follow: sub-row remainder (`sidebar_px`)
  plus clamp stepping through `scroll_to_item` in the workspace wheel
  path. The handle's own top-index getter is test-gated upstream, so the
  rail carries a `sidebar_top` mirror stepped beside every programmatic
  scroll.
- Stabilizer repair (compile + one red test): the 4A refactor had moved
  the discard two-press arm ahead of the untracked-file creation refusal,
  so `diff.discard-hunk` armed a live question on an unservable hunk.
  The arm now lives in `submit_hunk_patch` after the refusal (keyboard
  passes its row, the strip passes `None` — it never discards), restoring
  the original refuse-before-arm order.

DONE (resume `desktop-v2 phase4A-resume`):

- `sidebar_top` reconcile: `reconcile_top` (`views/workspace.rs`) adopts
  the handle's settled pixel offset at each wheel decision — except while a
  programmatic request is still parked, when the mirror names the intent and
  stays authoritative. The wheel path also drops the banked remainder when
  another path moved the list, so the next flick starts fresh instead of
  jumping. (There is no sidebar scrollbar element to drag; the bypass path
  that actually exists is the keyboard-follow `Nearest` scroll.)
- Strict Top: the wheel path parks `scroll_to_item_strict`, and the settle
  arithmetic lives in pure `wheel_step` — `None` banks sub-row pixels,
  clamping forgets the remainder. Tests:
  `a_step_onto_an_already_visible_row_still_moves_the_top`,
  `the_mirror_follows_a_scroll_it_did_not_issue`,
  `sidebar_steps_park_strict_requests` (contract pin on GPUI's strict).

## Phase 4B status (landed): visual fidelity

Commit `desktop-v2 phase4B: visual fidelity`: shell 408 (incl. new
`workspace_rows_are_two_lines_plus_air`), app 155, core 498;
`cargo check` zero warnings; `cargo fmt --check` + `cargo clippy
--workspace --all-targets -- -D warnings` clean; `./check.sh` all green
(incl. the real-fixture diff pipeline).

DONE:

- Contrast audit: `Theme::guide()` passes every floor with headroom —
  only two `*` marks, both the mechanism working as designed (gutter
  lifted per-surface to >=3.01 everywhere; Comment lifted to >=3.5).
  Strictly cleaner than `dark` (which lifts five syntax classes on
  MovedRemoved). No theme value changed. Subdued surfaces hold:
  title_bg 1.05, status_bg 1.01 vs bg; edges are `border` hairlines.
- Font decision (the open question since Phase 1): dual-face via
  `Host.chrome_family` (default `"SF Pro Text"`, `[font] chrome_family`
  knob, dump/apply round-trip tested). The spacing ladder, gutters and
  truncation stay on the mono advance — the chrome face is a name only,
  never measured. Workspace chrome (sidebar, inspector, toolbar chips,
  headers, center header) draws in it; diff rows keep `font.family`, so
  space-aligned columns cannot shear. Face resolution itself needs one
  look at a running window (headless builds cannot prove a family name).
- Sidebar rows rebuilt in the reference's shape: 48px two-line rows
  (filename over dim directory) from `ws_row_h` (scales with settings),
  14px boxes with 4px radius, partial boxes in accent on accent borders,
  accent `n/m` fractions, small ink status letters, green-tint selection
  with rounded ends and no keyboard bar, sentence-case muted group
  headings with counts, 35px nav with tinted actives, repo mark + name +
  path identity, bordered filter box, utilities hairline, 18px rails,
  staged/total hunk footer from the refresh's own counts map.
- Inspector: staged-count pill (accent on green tint), slimmer STAGED
  FILES caption, composer hairline above the fields.
- Center header 40->48px with the segmented Unified/Split control
  (tinted pill, surface chosen); workspace header with 19px semibold
  title, 22px insets, working-copy dot sentence on the right.
- Responsive: `narrow` under 1150px hides the branch chip's `from <base>`
  at composition time (never in a view); 1550px 280/295px rails already
  held. The 850px composer move stays a browser reference — the desktop
  keeps its inspector and scrolls.

DEVIATIONS FROM THE MOCK (deliberate, each with its reason):

- Checked boxes read from the accent fill alone — no check glyph, because
  no icon-font codepoint is safe in an arbitrary configured face.
- Headings share the 48px file-row slot (centered): `uniform_list`
  virtualizes one height, and the mock's group gaps absorb the air.
- No diff-summary strip: status word + totals live in the 48px center
  header rather than a second band — one band names the file.
- 18px sidebar rails vs the old stack's 10px ROW_PAD axis (workspace-only;
  the numbered stack keeps its axis until Phase 5).
- Sidebar rows are 48px at 15px against ~50px at 13px in CSS: same
  proportions, GPUI-measured. No CSS pixel was copied anywhere.
- The exact chrome-face rendering ("SF Pro Text" resolution) is
  unverified headless — first window look confirms or corrects it.

## Native-keys shell, option B (landed): command keys + help panel removed

The desktop no longer drives commands from single keys. Arrows/Tab/Enter/Esc,
text editing, menus, and the Commands palette stay; `on_key` keymap
resolution, the modes stack, `dispatch::translate` (+tests), `help.rs`, the
`?` binding, and `[keys]`-as-driver are gone. shell 371 · app 155 ·
core 498 green; fmt + workspace clippy `-D warnings` clean. `core`/`app`/
TUI/CLI untouched — the registry, bindings, and keymaps still serve them.

- Removed: `shell/src/dispatch.rs`, `shell/src/help.rs`, modes-stack
  driving, `sync_modes`, `pending` chords, `Pane::mode` readers
  (`Screen::mode` is `cfg(test)` now; the trait seam keeps
  `#[allow(dead_code)]` like `label`), status-bar hint projection
  (`hints`, `hints_budget`, `version`, badge consts), `MODE` consts
  (input/panes/settings), `leave`-adjacent key docs across shell.
- Restored after an over-broad cut: `cycle_pane`/`pane_walk` + the four
  `pane.*` arms (pure focus cycling, palette-runnable; TUI still owns the
  names), with de-keyboarded docs.
- Rewired, not removed: settings window on native keys (fixed exits +
  arrows/Tab/Enter/Esc); reset question answered from Commands
  (`reset to {}? Commands: soft · mixed · hard · esc cancels`);
  `commands.palette` is a real filter dialog (toolbar button, Ctrl-K,
  macOS Cmd-K menu adapter).
- Confirmation flows (discard two-press, reset answers) still run through
  named commands, so the palette answers them — and the band now answers
  them too (see confirm-buttons record below).
- Reverted out of scope: a `vercel` theme + mock restyle the run produced
  unasked (`core/`, `app/`, `artifacts/`, `docs/` restored verbatim).
- Tests: command-key driving + help/context-menu tests deleted with
  reason; focus/esc/search/commit/tag/reset/discard behavior kept via
  `run_command`; reset copy updated to match.

## Confirm buttons (landed): standing questions answer by click

The status band renders one button per answer beside a standing question
plus Cancel. shell 373 (incl. 2 new) · app 155 · core 498 green; fmt +
workspace clippy `-D warnings` clean. `core`/`app` untouched.

- `Notice::Question` carries `answers: Vec<Answer>` (label + command
  name, both `&'static str`) beside the text; `text()`/`Deref` unchanged
  so text-matching tests were untouched.
- `run_command` attaches answers from the asking command via
  `question_answers` (discard-hunk/file, branch delete, stash drop,
  squash/fixup/drop-commit, rebase-onto answer themselves; reset-menu
  arms soft/mixed/hard). Freshness-guarded: only a newly-asked,
  still-answerless question is annotated, so stale text never gains
  another command's buttons. Unmapped commands ask text-only.
- Cancel runs `back` — the Esc path — and `back` now dismisses a standing
  question (clearing its text and disarming the commits timeline, as Esc
  already disarmed it). Clicking an answer === running that command:
  same arm/execute, cursor-move disarm, verbatim errors.
- No copy changes: "press again" names no key, and Esc is a kept native
  key, so every question text still reads true.
