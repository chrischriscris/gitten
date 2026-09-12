# TUI Lazygit parity: implementation handoff for a model swarm

## Objective and authorization

Implement the missing keyboard-driven Git workflows in **gitten-tui**, from
essential daily operations through Lazygit's advanced and minor use cases.
The user explicitly named the TUI, asked for a comparison with Lazygit, then
requested this handoff for a model swarm to implement. This file is the handoff;
its creation did not implement any product changes.

Functional parity means that a user can discover, navigate to, execute, and recover
from an operation inside the TUI. A backend method, a registered command name, a
help entry, or an escape to a shell is not evidence that a built-in workflow is
complete. Keyboard compatibility is a separate acceptance dimension: document and
test the actual default keys, mode precedence, and deliberate differences.

Build the implementation only in the TUI. Add shared models, repository methods,
and actions where needed. Keep other clients compiling and behaving as before;
do not add their versions of these features unasked.

## Baseline and first reads

Prepared 2026-09-06 against HEAD
`e016e380376fc898238b875a0e4e4aacfaf5da0c` plus local changes. The earlier audit
started at `aadf684`; another actor advanced the checkout during this conversation.
At the final planning inspection `.gitignore`, `Cargo.toml`, `dev`, and
`docs/measurements.md` were modified. Treat these as user-owned changes, not plan
work. Do not reset, stash, commit, or overwrite them as part of setup.

The audit ran `cargo test -p gitten-tui --locked --offline -q`: 177 library tests
and 62 binary tests passed. That is a historical result on the audited working
tree, not proof for a future checkout. No interactive client was launched.

Read before editing:

- `AGENTS.md`, `docs/README.md`, `docs/clients.md`, `docs/architecture.md`.
- `docs/terminal.md`, `docs/extending.md`, `docs/diff-pipeline.md`.
- `docs/worktrees.md`, `docs/decisions/0025-formatting-and-lints-are-gated.md`.
- `docs/roadmap.md` for context, but trust live code over roadmap claims.
- `core/src/command.rs`, `app/src/act.rs`, `app/src/verbs.rs`,
  `app/src/jobs.rs`, `git/src/lib.rs`, `tui/src/main.rs`.

First commands, all read-only:

```sh
git status --short
git rev-parse HEAD
git diff --stat e016e380376fc898238b875a0e4e4aacfaf5da0c..HEAD -- core app git tui docs
git diff --stat
git diff --cached --stat
cargo test -p gitten-tui --locked --offline -q
```

Compare the symbols below against live code. Ordinary drift calls for reconciling
the packet, not abandoning the task or reimplementing a feature that has landed.
An overlapping uncommitted edit must be isolated or coordinated before mutation.
Missing cached dependencies are an environment issue; do not update the lockfile
to make an offline command pass.

## External reference and parity ledger

Use Lazygit's official sources, not memory or third-party shortcut lists:

- https://github.com/jesseduffield/lazygit/blob/master/docs/keybindings/Keybindings_en.md
- https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md
- https://github.com/jesseduffield/lazygit/blob/master/docs/Custom_Command_Keybindings.md
- https://github.com/jesseduffield/lazygit/blob/master/README.md

W0 must resolve one upstream commit or release and record it in
`plans/tui-parity-ledger.md`, replacing moving links with pinned links. The
reference must remain stable for the implementation campaign. Inventory every
action in its keybindings and documented workflow families, grouping aliases into
one action. Do not copy the full upstream documentation into this repository.

Ledger columns: ID, user workflow, upstream source, importance, packet owner,
Gitten command, default key/mode, current status, implementation evidence,
behavioral test, and remaining limitation. Use `MISSING`, `PARTIAL`, `IMPLEMENTED`,
`VERIFIED`, or `EXCLUDED (reason)`. An exclusion requires an explicit product
decision; platform-specific or Git-flow conveniences must not silently disappear
because the initial audit did not enumerate every key. Functional equivalents
are allowed, but record shortcut differences separately.

This packet list is a prioritized starting inventory, not a claim that an audit
of a moving upstream reference was exhaustive. Assign newly discovered omissions
to an existing packet or a numbered follow-up with dependencies. Full parity may
only be claimed against the pinned ledger with every in-scope row verified.

## Verified current state

Line numbers are discovery aids; symbols are authoritative after drift.

| Area | Current behavior and evidence |
|---|---|
| Command dispatch | `App::dispatch_to`, `tui/src/main.rs:2437`, handles file, basic branch, stash and hunk stage/unstage actions. Unhandled names fall through to `Screens::run`, then report `does nothing here`. |
| Navigation | `Screens::run`, `tui/src/main.rs:805`, implements row/page navigation and diff file jumps. `core/src/command.rs:338` defines defaults. |
| Preview | `App::sync_main_diff`, `tui/src/main.rs:2723`, always reads the named commits pane. Files, branches and stashes do not drive selected-resource previews. |
| Diff sources | `Repo::pairs`, `git/src/lib.rs:1016`, uses `git diff HEAD` for an empty revspec: HEAD versus working tree, not separate staged/unstaged comparisons. |
| Hunk writes | `hunk_action`, `tui/src/main.rs:3331`, accepts only empty-revspec repository diffs and emits a complete hunk. New untracked-file hunks are refused. No line selection or discard-hunk dispatch. |
| Existing actions | `app/src/act.rs` centralizes file/branch guards, confirmation and job submission through client traits. Extend this pattern rather than duplicating desktop policy. |
| Existing jobs | `app/src/verbs.rs` already has sync, reset, revert, cherry-pick and rebase jobs. Their existence does not imply TUI reachability. |
| Search | Only commit search is routed. `Commits::apply_query`, `tui/src/commits.rs:327`, filters loaded commits while retaining selection identity where possible. |
| Input | `apply_edit`, `tui/src/main.rs:899`, appends/removes at the end; pasted newlines become spaces. Amend starts empty. |
| Stashes | Push without a custom message; apply/pop/drop work. No selected-stash contents preview, advanced stash choices or rename. |
| Branches | Checkout/new/rename/non-force delete work; new branch is at HEAD without checkout. Remote-ref checkout detaches HEAD. Lightweight tagging of a selected local branch works. |
| Help | `tui/src/help.rs` projects the shared key registry without filtering unsupported TUI commands. |
| Known no-ops | Fetch/pull/push, manual refresh, file/branch/stash search, history editing and rebase lifecycle. `rebase_commands_remain_explicitly_deferred`, `tui/src/main.rs:9144`, explicitly tests some no-ops. Replace this test when implementation lands. |

Two load-bearing excerpts to confirm at kickoff:

```rust
// git/src/lib.rs, Repo implementation: the current aggregate source
let raw = if revspec.is_empty() {
    run(&self.root, &[&["diff"], &RAW[..], &["HEAD"]].concat())?
```

```rust
// tui/src/main.rs, dispatch_to: current hunk write routing
"diff.stage-hunk" | "diff.unstage-hunk" => self.hunk_verb(command),
```

The combined-source/index mismatch is evidenced by code inspection. No failing
mixed-index repository scenario was executed during the audit: W1/W3 must reproduce
and characterize it before replacing the path.

## Architectural requirements

1. `core/` stays dependency-free and I/O-free. Selection, operation state models,
   patch construction, command names, navigation policy and reusable models belong
   there. Do not add Git or terminal dependencies.
2. `gitten-git::Repo`, behind `Handle`, is the repository boundary. Add object-safe
   methods there; use the git binary for writes and current acquisition machinery
   for reads. There is no gix handle. Do not parse Git output or spawn Git in TUI.
3. Shared action policy belongs beside `app::act`; jobs and acquisition beside
   `app::verbs`, `app::jobs`, `app::acquire`. Clients supply selection, input,
   rendering and job submission. Shared behavior must be reachable through the
   same seam from another client or a compiled-in extension.
4. A key is data, a command is a name. No frontend match on raw keys implementing
   Git actions. Terminal text editing may translate physical editing input, but
   reusable editing/navigation semantics must not become TUI-owned policy.
5. Preserve raw path/ref bytes. Use lossy decoding only for display. Reads from
   Git must not require UTF-8. Keep `--topo-order` for history traversal.
6. Diffs come from acquired content and the shared differ pipeline. Never replace
   it with Git-generated display diffs. Preserve OID/settings cache identities;
   working-tree and selection-dependent data need correct invalidation.
7. Writes serialize through the existing queue; success and refusal refresh
   affected state. Capture repository, source, target and generation for an action.
   Revalidate stale targets before mutation, and never silently retarget a command
   after selection, repository or operation state changes.
8. Use existing extension seams; introduce a trait/registry when behavior should
   be replaceable. Do not build an external plugin loader merely to ship this plan.
9. Keep idle rendering idle, retain virtualized/visible-row drawing and cache
   derived state outside render. Benchmark release binaries only. Mac first,
   portable mechanisms where available; do not introduce unnecessary OS coupling.
10. Never run bare `cargo update`, bump GPUI or change the toolchain for this work.
    Do not launch an interactive client without a separate user request. Headless
    tests and dump frames are the verification path provided here.

## Swarm execution contract

Use one coordinator/integrator and up to three workers concurrently. The coordinator
may dispatch a fresh read-only reviewer after a worker finishes. More agents do not
make concurrent edits to central Rust modules safe.

- Coordinator owns this plan, `plans/README.md`, the ledger, contract decisions and
  integration. Workers report results; they do not independently mark a wave done.
- Use isolated feature worktrees under `.worktrees/<slug>` per `docs/worktrees.md`.
  Base each wave on the integrated predecessor, not on the original audit SHA.
  Preserve the user's dirty checkout. Do not manufacture a baseline commit containing
  user edits. If needed, work from committed HEAD and explicitly report the difference.
- Each packet owns its feature-specific modules and tests. The coordinator grants
  explicit, time-bounded ownership of `core/src/command.rs`, `core/src/host.rs`,
  `git/src/lib.rs`, `app/src/{act,verbs,acquire,jobs}.rs` and `tui/src/main.rs`.
  A worker without ownership submits a small integration patch/contract proposal.
- Freeze shared API contracts at each wave boundary. Independent work may proceed
  on models, tests, views and documentation after agreement; dependent integrations
  stay sequential. Avoid a wholesale dispatcher refactor as a prerequisite.
- A shared Cargo target directory saves disk but Cargo may serialize builds. The
  coordinator schedules full checks. Never run fixture-mutating `./dev check`
  concurrently against the same worktree.
- A worker handback includes commit/diff, changed paths, commands and actual results,
  new command/key inventory, edge cases, performance evidence where relevant, and
  remaining gaps. No pushes, publishing, remote writes or PR merges are authorized
  by this document. Local integration must preserve unrelated changes.
- On contract conflict, report to the coordinator and continue independent work.
  Ask the user only for a material product decision that cannot be resolved by
  these requirements. Routine implementation choices are the swarm's responsibility.

Suggested dispatch text:

> Implement packet Wn from plans/001-tui-lazygit-parity-swarm.md. Read that full
> file, AGENTS.md, and the packet's named sources. Work only in your assigned
> worktree and ownership scope. Confirm dependency contracts before editing shared
> files. Deliver working code and the packet's behavioral tests, not a stub or a
> new plan. Do not launch clients or touch real repositories/remotes for testing.
> Return evidence and remaining limitations; the coordinator owns integration.

## Work packets and dependencies

Effort is coarse: M = roughly a day of engineering; L = multiple days; XL = a
substantial feature family that must be divided into reviewed vertical slices.
These are not promises about model runtime. All packets start TODO.

| Packet | Outcome | Depends on | Effort / risk |
|---|---|---|---|
| W0 | Pinned ledger, command availability, refresh and regression baseline | — | M / medium |
| W1 | Explicit diff sources and correct selected-resource previews | W0 | L / high |
| W2 | Complete keyboard navigation, search and prompt editing | W0; integrate source modes with W1 | L / medium |
| W3 | Correct hunk/line/range stage, unstage and discard | W1, W2 | L / high |
| W4 | Remote sync, tracking branches and repository switching | W0, W2 | L / high |
| W5 | Operation lifecycle, merge and conflict resolution | W1, W2 | XL / high |
| W6 | Reachable history operations and interactive rebase | W3, W5 | XL / high |
| W7 | Complete stash, refs, tags, reflog and recovery | W1, W2, W4, W5 | XL / high |
| W8 | Custom patches, fixup targeting and advanced history surgery | W3, W6, W7 | XL / high |
| W9 | Worktrees, submodules and specialized repository workflows | W4, W5 | XL / high |
| W10 | External tools, custom commands and remaining conveniences | W2, W4; W5 for conflict tools | L / high |
| W11 | Cross-workflow parity verification, usability and performance | W0–W10 | L / medium |

Recommended waves: W0; W1+W2; W3+W4+W5; W6+W7+W9 where dependencies permit;
W8+W10; W11. W10 can start earlier when ownership is disjoint. Dependencies govern
dispatch, not the visual order of this suggested schedule. Do not stop the whole
campaign after essentials without explicitly reporting the unimplemented ledger.

### W0 — Establish truthful capabilities and refresh

Owner scope: `core/src/command.rs`, `tui/src/help.rs`, TUI dispatch/refresh tests,
and coordinator-owned ledger integration.

1. Freeze the upstream reference and inventory every action. Verify the baseline
   above against current dispatch and tests. Record existing functionality too.
2. Add a shared representation of command availability supplied by the client:
   distinguish unsupported from supported-but-currently-disabled with a reason.
   Derive help and dispatch refusal from the same contract. Preserve registered
   extension commands and existing mode precedence. Never advertise a no-op as usable.
3. Route `repo.refresh` through existing generation/reload machinery. Preserve
   selected identity and viewport where possible; refresh hidden panes too.

Acceptance: headless key tests show R rereads externally changed fake repository
state; all advertised TUI commands have a handler or explicit disabled reason;
fixtures reject repository operations; remapped keys and help agree; refresh
failures preserve the last good rows and surface the error. Run core/app/TUI tests.

### W1 — Separate diff sources and connect previews

Owner scope: shared diff/source models, `git::Repo`, `app::acquire`,
`tui/src/{main,files,branches,stashes,diff}.rs` through allocated integration slots.

1. Define explicit source identity: HEAD→index (staged), index→worktree (unstaged),
   untracked creation, commit comparison and stash comparison. Retain path identity,
   OIDs/content identity and write eligibility; never infer eligibility from an
   empty string. Preserve existing CLI revspec behavior for aggregate read-only views.
2. Acquire content through Repo, including unborn HEAD, deleted/renamed files,
   binary/type/mode changes, untracked files and stash untracked content. Reuse shared
   differ/assembly. Mark unsupported content kinds explicitly instead of inventing text.
3. File selection previews that file and side. Enter focuses its diff; Tab switches
   staged/unstaged with source-aware state. Branch selection supports history drilldown;
   stash selection supports diff and file drilldown. Esc returns through the actual
   navigation stack without switching to an unrelated commit preview.
4. Do not let slow reads overwrite a newer selection or another repository. Keep
   acquisition outside render and input responsive while a large preview loads.

Acceptance: a scratch file with HEAD=A, index=B, worktree=C yields precisely A→B
and B→C previews; moving files/branches/stashes targets the correct content;
out-of-order completion and repository switch cannot install stale previews;
unborn and empty repositories are usable. Verify core/app/git/TUI tests and
headless source labels/focus/empty/loading/error frames.

### W2 — Complete keyboard navigation and input

Owner scope: `core/src/{command,search,select,view}.rs`, reusable input model if
needed, `tui/src/{term,help,commits,files,branches,stashes,diff,panes}.rs` and prompt glue.

1. Extend live search to every relevant list and diff; support next/previous match,
   clearing/cancel, no results and stable target identity. Search loaded versus all
   history must be stated honestly; log/path/author filters belong in shared acquisition.
2. Add hunk jumps, keyboard range selection, list multiselect where actions support
   it, selection clearing and staged/unstaged modes. Keep text-copy selections distinct
   from operation selections. Preserve row identities through wrap and split layouts.
3. Complete prompt editing: cursor movement, Home/End, deletion, paste and multiline
   commit messages; prefill amend from HEAD. Keep prompt input isolated from global
   commands, including pasted q, newlines and escape sequences.
4. Use Lazygit-compatible context keys where feasible; document conflicts with existing
   Gitten defaults and preserve user overrides. If needed, introduce a shared named
   keymap preset rather than a private terminal key table. Include tab navigation,
   page/boundary behavior, focus return and help scrolling in the compatibility ledger.

Acceptance: key-event sequences assert selection ranges, focus, search results and
prompt text; remapping changes both execution and help; narrow terminal and Unicode
text tests pass; changed rendering still uses cached row/index data. Run core/app/TUI tests.

### W3 — Make partial staging correct

Owner scope: `core/src/patch.rs`, shared operation-selection model, Git patch verbs,
`app::act`, TUI diff action integration.

1. Reproduce mixed staged/unstaged changes in one hunk before changing behavior.
2. Construct stage patches from index→worktree and unstage patches from HEAD→index.
   Implement selected line/range and whole-hunk operations through one shared patch
   seam. Confirm discard against the exact unstaged target; staged discard offers
   unstage explicitly. Reject stale source content instead of applying to a new target.
3. Handle context, insertions, removals, replacements, file boundaries and final
   newline markers. Support creation where valid; show explicit whole-file alternatives
   for binary/type/mode changes that cannot be represented as a selected text patch.
4. Refresh both sides after writes and retain useful keyboard position. Expose
   hunk editing via W10 once the external-editor lifecycle exists.

Acceptance: real scratch-repository tests assert exact HEAD, index and worktree
bytes after stage/unstage/discard of a subset of one replacement hunk; untouched
changes remain untouched. Cover additions, deletions, missing final newline,
non-ASCII content, raw-byte paths, overlapping edits and stale previews. Run
core/app/git/TUI tests; do not settle for assertions that only inspect Git arguments.

### W4 — Complete sync and branch/repository movement

Owner scope: `app/src/{verbs,act,projects,acquire}.rs`, Repo sync/upstream methods,
TUI sync/progress and branch/project input modules.

1. Wire existing fetch/pull/push jobs. Add remote/upstream selection and clear
   no-upstream/no-remote handling. Preserve configured hooks, credential helpers and
   SSH behavior. Jobs must report progress/error and not freeze terminal input.
2. Support tracking-branch creation from a remote ref, branch creation at a selected
   commit/ref with an explicit checkout choice, checkout by name/previous branch,
   upstream set/unset and fast-forward workflows. Destructive variants need explicit
   confirmation; never silently force a rejected push or checkout.
3. Wire existing project MRU/path services to repository switching. Prevent pending
   jobs/results from leaking across repository identity; keep old work owned by its
   repository until settled. Preserve navigation/config behavior on failed opens.

Acceptance: local bare remote plus two scratch clones exercise fetch, first push,
pull, rejected push, tracking setup and detached HEAD. Fake blocking jobs prove
input stays responsive and errors visible. Switching repository during a pending
result cannot alter the new repository or install old content. Run app/git/TUI tests.

### W5 — Implement operation state and conflict recovery

Owner scope: `core` operation/conflict models, Repo state acquisition/lifecycle,
`app::act`/jobs, new TUI operation/conflict views.

1. Model merge, rebase, cherry-pick and revert state, including externally started
   operations. Read through Repo; never inspect `.git` directly in a client or assume
   `.git` is a directory (linked worktrees exist).
2. Provide merge options and operation-specific continue/abort/skip when valid.
   Do not equate all conflicts with rebase state. Gate invalid actions with reasons.
3. Resolve individual text conflicts with ours/theirs/both and manual edit integration;
   support whole-file choices and clear fallback for binary, delete/modify and rename
   conflicts. Preserve unresolved content; expose local resolution undo accurately.
4. Refresh operation state after each write/refusal and after manual refresh. Closing
   and reopening the application must recover the operation UI from repository state.

Acceptance: scratch repositories deliberately stop merge/rebase/cherry-pick/revert
on conflicts; each can be resolved and continued or aborted through TUI commands.
Assert actual repository state and bytes, not just banners. Operation-specific skip
is offered only where supported. Run core/app/git/TUI tests.

### W6 — Expose history editing and rebase

Owner scope: `core/src/rebase.rs`, shared history actions/Repo methods, new TUI
history/todo UI and command integration.

1. Wire reset soft/mixed/hard, revert, cherry-pick and detached commit checkout.
   Add multicommit copy/paste semantics with stable IDs and explicit ordering.
2. Wire squash/fixup/drop and rebase-onto through existing `TodoScript` and jobs.
   Replace deferred-no-op tests with real behavior and cancellation/refusal tests.
3. Implement editable todo operations: pick/reword/edit/squash/fixup/drop/reorder,
   fixup message variants, autosquash and rebase base/onto selection. Honor the shared
   model's constraints; root/merge cases need actual support or an explicit ledger gap.
4. Use W5 for conflicts and external editor pauses. Confirm rewrites with precise
   targets; never drop commits merely to approximate an unsupported operation.

Acceptance: small DAG fixtures assert parent relationships, resulting messages,
tree contents and selection after each operation. Include root, merge, empty commit,
dirty tree, invalid reorder and conflict cases. Reset tests distinguish index and
worktree outcomes. Run core/app/git/TUI tests.

### W7 — Complete stash, refs and recovery

Owner scope: `core/src/refs.rs`, Repo stash/tag/reflog methods, shared actions and
TUI stash/ref/reflog views.

1. Add named stash, staged/unstaged/selected-file/untracked variants, rename and
   branch-from-stash. Inspect content before applying. Target stable stash identity,
   not a stale numeric index after stack changes; preserve stash on failed pop.
2. Add tag list/checkout/create annotated or lightweight/delete/push, remote list
   add/edit/remove/fetch, remote-branch deletion, and branch/ref history navigation.
3. Add reflog browsing and recovery plus undo/redo with honest operation boundaries.
   Do not promise recovery of discarded uncommitted bytes unless actually retained.
   Give a preview/confirmation of the proposed recovery target and worktree effects.

Acceptance: scratch tests cover each stash variant without stealing excluded changes,
stash reorder races, conflicted pop, tags/remotes against local bare repositories,
and reflog recovery after reset/rewrite. Test UI keys as well as Repo methods.
Run core/app/git/TUI tests.

### W8 — Advanced patches and history surgery

Owner scope: shared patch-selection/rebase models, Repo history verbs, app orchestration,
TUI patch builder and history targeting views.

1. Build reusable selected-file/hunk/line custom patches across commits.
2. Support applying/reversing patches, removing selected changes from a historical
   commit, amending a selected historical commit, and moving selected changes or
   commits to another/new branch. Distinguish patch clipboard from text clipboard.
3. Implement fixup creation/target discovery and autosquash flows represented in the
   pinned ledger. Preserve commit ordering and use the operation lifecycle for conflicts.

Acceptance: DAG and byte-level tests prove only selected changes move, later dependent
commits either replay correctly or stop in a recoverable state, and cancellation
does not mutate history. Cover cross-file patches and rename boundaries. Each slice
must have a TUI key sequence, not just a script API. Run core/app/git/TUI tests.

### W9 — Worktrees, submodules and specialized workflows

Owner scope: new shared models and Repo methods, app actions, TUI list/form modules.

1. Implement worktree list/create/switch/remove and open-in-editor handoff. Include
   dirty worktrees, locked/prunable entries and branches checked out elsewhere.
2. Implement submodule list/enter/return/add/init/update/remove/URL and bulk operations
   identified by the ledger. Handle detached submodule HEAD and nested repository context.
3. Implement bisect start/good/bad/skip/reset with persisted state and checkout effects.
   Inventory git-flow and other specialized upstream actions; implement equivalents
   or obtain a documented scope decision rather than omitting them silently.

Acceptance: nested scratch repositories exercise worktree/submodule transitions,
dirty deletion refusal and exact path targeting; bisect reaches a known introduced
commit and reset restores the original state. No test operates on the developer's
real worktrees. Run core/app/git/TUI tests.

### W10 — External tools and remaining conveniences

Owner scope: shared tool/custom-command configuration and execution services in app,
core command/context contracts, TUI terminal suspend/restore glue and tool menus.

1. Support configured editor, file opener, difftool/mergetool, commit editor and hunk
   editor. Restore terminal modes on success, error and interruption, then refresh.
2. Add custom commands with selected-context parameters, prompts, menus, output and
   foreground/background behavior. Keep argument invocation separate from explicitly
   configured shell snippets; never interpolate selected paths into shell source.
   Reuse the named command/config/extension seam, not a parallel keymap.
3. Complete ledger conveniences: command log, diff options (context/whitespace/
   reverse/ref comparison/renderers), file tree and collapse/expand, file/status/log
   filters, commit attribute editing/copy, browser/PR URLs, screen modes, config
   access and other upstream actions found in W0. Split into independent slices.

Acceptance: fake tools record argv for paths with spaces, quotes and raw bytes;
headless/private-PTY tests verify terminal restoration without taking over the user's
terminal. Tests never launch real editors/browsers or publish PRs. Custom commands
are discoverable and remappable, cancellation invokes nothing, failures retain useful
output. Run core/app/git/TUI tests plus focused terminal lifecycle tests.

### W11 — Verify parity as workflows

Owner: independent reviewer followed by coordinator integration fixes.

1. Walk every ledger row using TUI key events and a real scratch Repo where it mutates
   Git. Verify actual state and user-visible feedback. Missing test/handler/entry point
   means the row is not VERIFIED.
2. Test complete journeys: inspect→partial stage→commit→push; fetch→tracking checkout;
   stash→switch→restore; conflict→resolve→continue; cherry-pick range→conflict→abort;
   custom patch→history rewrite→recovery; worktree/submodule entry→return.
3. Review headless frames at 120x40, 80x24 and a narrow size: prompt clipping, scroll
   position, selected-side labeling, disabled reasons, help, long paths, Unicode and
   errors. Preserve dense, quiet keyboard-first design. A passing state test alone
   does not prove legible layout.
4. Measure release fixtures before/after using existing harnesses. Record comparable
   commands and numbers in `docs/measurements.md`; investigate new render-time
   allocation/recomputation and input stalls. Do not fabricate thresholds or compare
   debug and release results.
5. Update system docs and actual key behavior, keep AGENTS philosophy-only. Deliver
   the pinned parity ledger with intentional differences and any unverified rows.

## Verification gates

Use existing fake Repo/App key tests in `tui/src/main.rs`, cell-buffer tests in
`tui/src/*`, and scratch-repository tests in `git/src/lib.rs` as patterns. Core
tests must remain windowless and dependency-free. Give new parity tests the
`tui_parity_` prefix so the coordinator can locate/run the campaign coverage.
Each acceptance behavior above needs a named test; printing success or inspecting
spawned arguments is insufficient for data-changing operations.

Packet tests (run the packages that packet changes; all must exit zero):

```sh
cargo test --locked -p gitten-core -p gitten-app -p gitten-git -p gitten-tui
cargo test --locked -p gitten-core -p gitten-app -p gitten-git -p gitten-tui tui_parity_
```

The filtered command can pass with zero tests: reviewers must verify that every
accepted ledger row names an executed test. Run once after each integrated wave:

```sh
cargo fmt --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Final headless release checks:

```sh
COLS=120 ROWS=40 ./dev dump diff --fixtures
COLS=80 ROWS=24 ./dev dump diff --fixtures
COLS=120 ROWS=40 ./dev dump commits .
GITTEN_TTI=0 ./dev check
```

`./dev check` temporarily rewrites generated fixture files, can read optional local
repositories and may fetch blobs from blobless ones. Run serially in the integration
worktree; inspect `check.sh` first. `GITTEN_TTI=0` disables its client-spawning timing
harness. Missing optional fixtures/platform dependencies must be reported as unrun
coverage, not waived as green. Keep all destructive tests hermetic in temporary repos;
use local bare remotes and explicit test identities, never real remote credentials.

Full cross-client compile/test checks protect the desktop without adding its new UI.
Linux verification uses existing CI where available; do not claim it ran locally on
macOS. If an unrelated baseline check fails, record it separately and still complete
all unaffected verification.

## Completion and escalation rules

A packet is done only when its user-facing path, shared seam, semantic tests,
headless visual evidence where applicable, docs, and ledger evidence are integrated.
A newly named command with a stub implementation is not progress eligible for DONE.
No entire-wave completion based on tests that deliberately assert missing behavior.

Pause the affected slice and tell the coordinator when a proposed implementation
would overwrite user work, violate a shared contract, require a dependency in core,
mutate an actual user repository during tests, or lose changes in a scenario whose
semantics are not understood. Continue independent work. Resolve routine drift and
test failures rather than repeatedly asking the user for permission.

Full campaign completion requires all W0–W11 packets and every in-scope pinned-ledger
row VERIFIED. If resources end first, report exact remaining packet/row IDs and
provide a resumable state; do not relabel essential-only coverage as full parity.

## Suggested skills for executors

- `tdd`: use for patch application, operation lifecycle and destructive Git flows;
  these need behavior-first regression tests against actual repository state.
- `diagnose`: use for concrete failing scenarios, stale-source errors or regressions.
- `setup-matt-pocock-skills`: read/run its required setup before first use of the
  above skills if the repository lacks their context; reconcile existing setup.
- `handoff`: preserve worker/coordinator state when passing to another session.

Do not use the read-only `improve` skill as an excuse to stop an executor at another
plan. This document is already the plan; dispatched workers implement their packet.
