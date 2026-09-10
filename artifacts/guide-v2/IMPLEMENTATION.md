# Desktop implementation specification

## Decision and scope

The compact workspace in this application is the accepted design concept. Implement this layout in the GPUI desktop product. Keep the system font, subdued surfaces, green selection, compact directory groups, and fixed right-hand commit inspector. The user explicitly rejected conversational copy: use ordinary application labels and factual status text.

Run the reference with `bun run dev` in this directory. Its only screen layout is the selected concept. Do not implement the web mock's fixture handlers in the real client.

Read the root `AGENTS.md`, `docs/README.md`, `docs/clients.md`, and `docs/architecture.md` before changing production code. They govern extension seams, ownership, and performance. The current application implementation is in `shell/src`; inspect the current code rather than assuming this document describes its internal structure.

## Window composition

| Surface | Placement and content |
| --- | --- |
| Toolbar | 53px high. System traffic lights, active branch, Commands, Push with outgoing count. |
| Sidebar | 255px at ordinary desktop widths. Repository identity, Changes/History navigation, Branches/Stashes controls, file filter, directory groups, staging checkboxes. |
| Main header | 76px high. Destination title, actual file/change counts, working-copy status. |
| Diff | Remaining center width. Path and Unified/Split controls, file status and totals, hunk controls, line numbers and syntax highlighting. |
| Inspector | 266px wide at ordinary desktop widths. Commit heading, staged-file summary, labeled Summary and optional Description, staged-hunk count, Commit action. |
| Status bar | 29px high. Fetch/push status, remote, staging count, keyboard commands. |

At widths above 1550px the reference uses a 280px sidebar and 295px inspector. CSS contains the remaining responsive dimensions. The browser moves the composer below the diff at 850px; treat this as a narrow-width reference, not a mandate to degrade desktop usability. Measure pane bounds rather than assuming every view owns the window.

The app occupies its full viewport: no page header, marketing text, guide annotations, floating comparison bar, or ornamental outer frame. macOS window chrome must use GPUI platform support rather than HTML's decorative traffic-light dots.

## Interaction contract

1. Changes is the default destination. History is a separate destination; it does not consume space in the normal changes workspace. The History timeline draws the same multi-lane commit graph as the commits list — branches fork and merge in its gutter rather than collapsing to a single rail — beside the selected commit's diff.
2. Files are grouped by directory. Directory labels appear once; each compact row has a leading stage checkbox, selectable filename, and trailing status. A mixed checkbox and hunk fraction represent partial staging. The reference's group chevrons are decorative; collapsing groups is not implemented here.
3. Selecting a file changes the diff only. Staging is a separate action. A click on a partially staged file's checkbox stages the remaining hunks; a fully staged checkbox unstages the file.
4. Hunk actions and file actions update the same index state. The right inspector lists staged files with paths and staged/total hunk counts.
5. Summary and Description survive navigation. Commit is disabled without both staged content and a non-whitespace summary. The browser shows a confirmation; production must commit only staged changes, retain unstaged changes, and update the view from repository state.
6. Keep Unified/Split in the diff toolbar and maintain selection when switching. Reuse the existing presentation registry, prepared rows, alignment, and diff cache.
7. Commands is reachable by mouse and keyboard. Preserve native input editing, visible focus, Escape dismissal, and focus restoration after dialogs. Resolve application actions through the existing named-command path.
8. Push displays actual outgoing counts and completion state. Branch and stash previews in the reference establish visual treatment only; preserve the production operations and their existing safeguards.
9. Show repository errors where the operation occurred. Do not replace hook failures, rejected pushes, partial staging, or conflicts with success copy.

## Implementation sequence

1. Inspect `shell/src/main.rs`, `chrome.rs`, `panes.rs`, `session.rs`, and `dispatch.rs`. Map the selected window structure onto existing entities, navigation, focus, and command dispatch before restructuring them.
2. Integrate the sidebar and directory-grouped file presentation with `shell/src/views/files.rs`. Shared grouping and selection state belongs below the client wherever another client or extension would need it. Do not add dependencies or I/O to `core`.
3. Reuse `shell/src/views/diff.rs` and the existing diff pipeline for the center surface. Maintain virtualized rows, fixed gutters, correct horizontal gesture ownership, and cached preparation.
4. Build the commit inspector against actual staged content and existing input/write paths. All mouse controls and shortcuts must invoke the same registered command names; do not create a parallel write implementation.
5. Apply the selected theme through the theme/configuration system, including widget synchronization. The CSS colors are design targets: resolve contrast against every actual diff background rather than copying literal colors into render methods.
6. Verify startup, selection, partial/full staging, unstage, commit, navigation, and real error states. Keep destructive-operation safeguards. Run relevant core/app tests and desktop compile checks through the repository's documented tooling.

## Production acceptance

- The selected layout, density, positions, and factual copy match the reference at desktop sizes.
- Files and diffs remain virtualized; no per-frame regrouping, diff preparation, or unbounded shaping.
- Extensions can contribute through existing or deliberately introduced shared seams.
- Real Git results drive counts and completion states, including after refused writes.
- Drafts and focus survive normal navigation; typing does not trigger unrelated shortcuts.
- Long paths, large diffs, empty status, partial staging, hook failure, push rejection, and conflicted status remain usable.
- Light/dark text contrast, scaling, resizing, and GPUI rendering are visually verified. The reference has not yet received browser visual QA.

The HTML fixture diff omits production features such as true line mappings, complete historical data, repository refresh, and durable drafts. These are not approved regressions. Preserve the existing application's capabilities when integrating this design. Do not launch the desktop client unless the user requests it; provide the launch command after build checks.
