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
2. **Phase 2:** workspace shell behind flag + sidebar grouped flatten + center
   header + single-file projection.
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
