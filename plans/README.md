# Implementation plans

| Plan | Objective | Status |
|---|---|---|
| [001 — TUI Lazygit parity swarm](001-tui-lazygit-parity-swarm.md) | Complete keyboard-driven Git workflows in the terminal, from everyday staging to advanced history operations | W0–W9 **merged to main** as #73 (squash `f1be454`); W10/W11 remain |
| [002 — W9: worktrees, submodules, bisect](002-w9-worktrees-submodules-bisect.md) | The three specialized repository surfaces gitten has none of | dispatch-ready, unassigned |
| [003 — W10a: external tools and custom commands](003-w10a-external-tools-and-custom-commands.md) | One execution service: editor, opener, difftool, shell, the user's own commands, the command log | dispatch-ready, unassigned |
| [004 — W10b: view conveniences and copy](004-w10b-view-conveniences-and-copy.md) | Diff and log options, the file tree, screen modes, the copy family — sixteen rows, no process spawned | dispatch-ready, unassigned |
| [005 — The window picks up what the campaign landed](005-desktop-picks-up-the-campaign.md) | Shared seams W0–W6 landed that the product cannot reach, and an audit of whether they are real | dispatch-ready, unassigned |

002–005 were written 2026-09-07 as parallel lanes for the same campaign: 002 and
003/004 are packets W9 and W10 of 001 at implementation depth, 005 is the desktop's
side of what 001 landed in the shared layers. Each names its own base commit,
worktree, owned files and marker blocks in the shared ones, so up to four can run
concurrently; the coordinator still owns integration order and the ledger.

Written 2026-09-06. This is an implementation handoff, not a record of completed features.
The plan contains the work packets, dependency graph, ownership rules, verification
requirements, and scope ledger. The swarm coordinator owns status updates.

Begin with packet W0. Do not dispatch every packet at once: later waves depend on
source identity, operation state, and shared action contracts established earlier.
