# Plan 015: Render the shared Markdown presentation in the terminal

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report; do not invent a second Markdown model
> in `tui/`. Do not update `plans/README.md`: the integrator owns the index for
> this plan.
>
> **Drift check (run first)**:
> `git diff --stat 67fee3d..HEAD -- core/src/markdown.rs core/src/rows.rs core/src/runs.rs shell/src/views/markdown.rs shell/src/views/diff.rs tui/src/lib.rs tui/src/rows.rs tui/src/screen.rs tui/src/diff.rs`
> If a cited current symbol changed, reconcile this plan with the live contract
> before editing. STOP if the reconciliation changes which layer owns layout or
> requires a dependency in `core`.

## Status

- **Priority**: P2
- **Effort**: L (multi-day: this is a shared-model extraction, two frontend
  adapters, cell rendering, and headless goldens; treating it as a one-session
  `Rows` copy would duplicate policy in `tui`)
- **Risk**: MED
- **Depends on**: none
- **Category**: direction / architecture
- **Planned at**: commit `67fee3d`, 2026-08-27
- **Confidence**: HIGH on the boundary and integration points; MED on the exact
  terminal substitute for a table hairline because terminals cannot paint
  between cell rows

## Why this matters

`docs/terminal.md`, under **Still to do**, predicts this work explicitly:
“`MarkdownRows`. `core/examples/paint.rs` already draws the furniture in ANSI,
so the terminal version is that function and a `Rows` impl — and the furniture
itself is then a fourth thing to lift into `core`.” The first half is now too
small a description: the window has since gained table squeezing, `Budget::At`,
selection, fixed-gutter horizontal panning, and table hairlines. Copying only
the ANSI example would produce a third answer to those decisions.

The desired result is that `.md`, `.markdown`, and `.mdx` files in the terminal's
`unified` layout use the same prepared text, block classification, marker
remapping, table flow, wrap-policy selection, and furniture description as the
window. The terminal owns only cell measurement, SGR/cell painting, and hit
translation. The `split` layout remains source-oriented in both clients.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -p gitten-core` | all tests pass |
| Terminal tests | `cargo test -p gitten-tui` | all tests and new frame goldens pass without entering raw mode |
| Desktop regression tests | `cargo test -p gitten-shell` | all tests pass headlessly |
| Formatting | `cargo fmt --check` | exit 0 |
| Lint | `cargo clippy -p gitten-core -p gitten-tui -p gitten-shell --all-targets` | no warnings |
| Full gate | `./check.sh` | exit 0; no client is launched |
| Optional full Markdown corpus | `./fixtures/fetch.sh` | creates ignored `fixtures/real/md.diff`; requires network and is not a test prerequisite |
| Optional release frame | `COLS=120 ROWS=40 FRAMES=200 ./dev --release dump diff --patch fixtures/real/md.diff` | prints one frame to stdout and timing to stderr; never grabs a tty |

## Scope

Only these files may change:

- `core/src/markdown.rs` — add the shared presentation state, semantic
  furniture, table-flow/wrap orchestration, and row-decoration query.
- `shell/src/views/markdown.rs` — replace its private layout state and decisions
  with the shared core model while retaining GPUI drawing and pixel measurement.
- `tui/src/lib.rs` — export the new terminal Markdown drawing module.
- `tui/src/rows.rs` — register the Markdown claimant in `unified` and expose a
  text/runs painter usable with shared Markdown row storage.
- `tui/src/markdown.rs` — new thin `Present`/terminal `Rows` adapter and cell
  painter.
- `tui/src/screen.rs` — only the minimal cell-bottom rule primitive described
  below, if the capability spike selects coloured underline.
- `tui/tests/fixtures/md.diff` — new, small, committed unified-diff slice for
  deterministic headless tests; it must not be the 70k-line ignored corpus.

Explicitly out of scope:

- `core/src/rows.rs`: its current `Present` contract is sufficient; do not add
  rendering, terminal units, or Markdown branches to it.
- `core/src/runs.rs`: consume `runs`/`runs_selected`; do not create a Markdown
  merge or alter run precedence.
- `shell/src/views/diff.rs`: its registry already registers the specialist after
  `TextRows`; keep that public behavior. Touch it only if drift makes removal of
  dead Markdown-local glue unavoidable, and STOP before doing so.
- `tui/src/diff.rs` and `tui/src/main.rs`: their generic reflow, layout/wrap,
  hit/select, and copy-on-select paths already provide the integration. Tests
  should exercise those paths without changing them.
- `app/src/config.rs` and all other `app/` files: no changes. Its `apply_diff`
  already writes `Host::layout` and selects `Host::wrap`; both clients already
  consume those fields.
- Fixing the known character-count versus display-column wrap limitation for all
  CJK prose. That is the separately recorded wrap-seam issue; do not smuggle it
  into this feature.
- A Markdown specialist in `split`, a new layout name, a settings key, a
  dependency in `core`, or any live window/tty test.

## Baseline facts (provenance)

### The architectural contract

- `AGENTS.md` says a client is drawing and input only, `core/` stays
  dependency-free, a wrapped line is more rows rather than a taller row, a grid
  is re-laid-out rather than broken, and per-line furniture changes each wrap
  budget. It also says fenced code is the only rendered code-block form:
  four-space indentation remains prose, and heading detection must count the
  original indent.
- `docs/clients.md` assigns presentation registries to clients, the selected
  layout *name* and wrap registry to `Host`, and column measurement to the
  frontend. It specifically contrasts window `Font::advance` with terminal
  `unicode-width`.
- `docs/terminal.md` predicts `MarkdownRows` and the furniture lift. It also
  records the terminal's fixed-gutter mechanism (`Pen::scroll`) and the known
  limitation that `core::wrap` currently counts characters, not display cells.

### What is already in core today

- `core/src/markdown.rs` already owns the structural model:
  `Block`, `Block::depth`, `Block::is_code`, `Block::is_table`, `TableGlyphs`,
  `Layout`, `lay_out_tables`, `Grid`, `Tables`, `TableRow`, `FlowRow`, and
  `flow_table`. `lay_out_tables` classifies each hunk side, strips Markdown
  markers, remaps tokens/spans, and records table runs. `flow_table` water-fills
  column widths, refuses a grid when pipes/padding leave less than one character
  per column, wraps cells, and returns one `FlowRow` per source line.
- `core/src/markdown.rs`'s private `classify` calls the shared
  `syntax::heading_level(line)` on the untrimmed line, so an indented `# comment`
  is not promoted to a heading. It treats only `fence_marker`-delimited runs as
  `Block::Code`; do not add indented-code behavior in a renderer.
- `core/src/rows.rs` defines the complete non-UI contract in `Present`:
  `claims`, `len`/`is_empty`, consuming `build(File)`, visual `rows`, approximate
  `width`, and `files`. `assemble` prepares once, gives each file to the last
  claimant, and builds the shared order table; `expand` asks each presentation
  for its visual rows and width. `reflow` deliberately stays in each frontend's
  trait because the frontend supplies units and owns the furniture.
- `core/src/runs.rs` owns `surfaces`, `runs`, and `runs_selected`: tokens and
  intraline spans become gapless line-coordinate `Run`s with a `Surface` and an
  optional syntax `Kind`. Selection outranks a changed-word surface. A terminal
  adapter must consume this output, not merge ranges itself.

### What remains window-side today

- `shell/src/views/markdown.rs` contains the current `MarkdownRows`. Although
  its module comment says the structural work is core-side, it still owns the
  presentation row store, distinct-block cache, per-block budget cache,
  table/grid indices, flowed-row store, and the orchestration in
  `MarkdownRows::reflow_tables` and `Rows::reflow` that chooses among
  `Budget::At`, `Budget::Cols(0)`, and an ordinary per-block column budget.
- That file also owns policy which a second client would otherwise duplicate:
  `Metrics`, `MarkdownRows::budget`, `MarkdownRows::furniture`,
  `MarkdownRows::ruled`, `MarkdownRows::text_of`, and the mapping from each
  `Block` to a bullet, bar, thematic rule, blank, table, heading, or fence
  label. These are the pieces to lift. GPUI-only remnants are
  `Metrics::for_font`, font-size/pixel conversion, `SharedString`,
  `AnyElement`, `StyledText`, div/canvas construction, and the actual 1px rule.
- `shell/src/views/diff.rs`'s `Layouts::builtin` registers `unified` as
  `TextRows` followed by `MarkdownRows` for `md`, `markdown`, and `mdx`, so the
  last claimant wins. It registers `split` as only `SplitRows`, intentionally.
  This is the behavior the TUI registry must mirror, not a new “markdown” layout.
- `core/examples/paint.rs` contains `furniture(Block, &MarkdownPalette, first)`
  and demonstrates ANSI bullets, quote/code bars, headings, and marker colours,
  but it calls `lay_out` and a single generic `Wrapped::build`. It predates
  shared `runs`, table flow, table hairlines, and selection, so it is a visual
  reference—not an implementation to copy verbatim.

### What the terminal already supplies

- `tui/src/rows.rs` defines terminal `Rows: Present` with `reflow` in columns,
  `render` into `Pen`, `hit`, `selectable`, and `report`. `Layouts::builtin`
  currently registers only `TextRows` under `unified` and `SplitRows` under
  `split`. `TextRows` is the model for delegation: `Present::build` pushes
  prepared data, `Rows::reflow` changes only wrapping, and `render` paints the
  visible row.
- `tui/src/split.rs` (not `tui/src/rows.rs`; the documentation's older path has
  drifted) contains the second `Present` implementation, `SplitRows`, and proves
  that a presentation can change row shape without changing `Diff`.
- `tui/src/screen.rs` supplies exact cell measurement through `cols`/`width`,
  SGR state in `Ink`, testable `Screen` buffers/`Screen::print`, and `Pen`.
  `Pen::scroll` swallows columns only after it is called, so a renderer can draw
  the gutter and Markdown furniture first and then pan text without slicing
  before tokens/spans are merged. `Pen::put` preserves the grid for wide and
  zero-width characters.
- `tui/src/diff.rs` needs no Markdown branches. `Diff::rebuild` uses the selected
  `Layouts` entry and shared `assemble`; `Diff::reflow` calls every owner's
  `Rows::reflow` and then `core::rows::expand`; `Diff::set_layout`/
  `cycle_layout` and `set_wrap`/`cycle_wrap` operate on registries. `Diff::locate`
  calls `Rows::hit`; `Selectable::text` calls `Rows::selectable`; `selection`
  and `copy_text` use `core::select`.
- `tui/src/main.rs` finishes copy-on-select on mouse-up and formats feedback in
  `copied` as `copied N lines`. Because it consumes `Screens::selection`, a
  correct Markdown `hit`/`selectable` implementation needs no main-loop change.

### Theme, cost, and fixture evidence

- `docs/theming.md` requires token text to use pre-resolved
  `Theme::syntax_on(kind, surface)` at `min_contrast` 3.5 and gutter furniture
  to use `Theme::gutter_on(surface)` at `min_furniture` 3.0. Markdown bars and
  markers come from `Theme::markdown`; line backgrounds still mean
  added/removed/moved/selected. `docs/syntax-highlighting.md` confirms Markdown
  tokens carry `Heading`, `Strong`, `Emphasis`, and `Link` and that fenced-code
  state is already handled by the Markdown highlighter.
- `docs/measurements.md` reports the rust-lang/book `md.diff` at 71,756 diff
  lines: `prepare` 90.7 ms, including 71.7 ms intraline; the shared Markdown
  `lay_out` pass is 5.6 ms for 71,705 Markdown rows (78 ns/row). Table flow adds
  1.0–1.4 ms at useful widths. The currently source-rendered TUI unified frame
  is 12 µs at 74,467 visual rows and 110 ms total load. The new TUI must pay the
  same one core prepare plus one shared layout pass as the window—never a second
  parse/layout pass—and should remain visible-row-bound per frame.
- Verified in this worktree: `ls -la fixtures/md.diff` fails, and the documented
  ignored `fixtures/real/md.diff` is also absent. `fixtures/fetch.sh` reproducibly
  creates `fixtures/real/md.diff`; tests must not depend on it or on network.

## Approach

1. **Extract one shared presentation model, not shared drawing.** Add a proposed
   `gitten_core::markdown::Document` (the exact new name may vary, but keep one
   type) that consumes prepared `File`s and owns rows, `Block`s, table indices,
   grids, flowed rows, wrap ranges, file entries, and the layout report. It must
   expose borrowed row/text/token/span data and ranges; clients must not clone
   line text per frame.
2. **Represent furniture semantically in core.** Add a proposed `Furniture`
   value/query derived from `Block`, visual segment, and neighboring rows. It
   describes indent depth, bullet depth/first-segment behavior, quote/code bar,
   heading level, fence-label treatment, thematic rule, blank suppression,
   table-grid treatment, and `rule_after`. It contains no GPUI types, pixels,
   terminal `Ink`, or ANSI. This is the required core-level addition for rows
   carrying markers; leaving `MarkdownRows::ruled` or the block-to-marker match
   independently in two clients fails the goal.
3. **Lift wrap-policy selection while keeping measurement injectable.** Move the
   current `reflow_tables` and `Wrapped::build_with` orchestration into
   `Document::reflow`. The caller supplies a pure `Block -> usize` text-budget
   function (or an equivalently small dependency-free measurement trait).
   Core decides per line: flowed table rows use `Budget::At`; intact/unflowable
   tables and headers use `Budget::Cols(0)`; ordinary rows use the supplied
   block budget. Core also decides when `rule_after` is true: only a data table
   row, only its final visual segment, only when another data row follows, never
   the separator or last row.
4. **Keep units at the adapter edge.** The shell adapter computes each block's
   character budget from the window width, fixed diff chrome, semantic
   furniture measured in pixels, heading size, and `Font::advance`. The TUI
   adapter computes it from terminal columns, its dynamic digit gutter, and
   semantic furniture measured with `screen::width` (`unicode-width`). Core sees
   only the resulting integer budgets and never learns “pixel”, “cell”, GPUI,
   or `unicode-width`. Display width still governs TUI hit testing, widest-row
   ranking, table/furniture painting, and horizontal bounds. The existing
   `core::wrap` character-break limitation remains explicit: wide prose may be
   clipped by `Pen`, as `docs/terminal.md` documents, until the separate wrap
   seam work lands.
5. **Use a cell-bottom decoration for table inner rules.** A terminal has no
   pixel between two rows. Do not insert a `─` row: that would invent a visual
   row with no source line and make gutter numbering/carets lie. First spike
   ANSI coloured underline (SGR 4 plus 58) on the final visual segment when
   core's `rule_after` is true. It preserves the glyphs, consumes no row, and is
   ignored gracefully by terminals lacking underline-colour support. If adopted,
   add the smallest `Ink` field/builder and `sgr` emission in `tui/src/screen.rs`;
   keep normal underline semantics intact. The shell continues to turn the same
   marker into its existing 1px bottom rule.
6. **Wrap the shared model twice.** Keep `shell::MarkdownRows` and add
   `tui::markdown::MarkdownRows` as frontend adapters because their `Rows`
   traits return different things. Both delegate `Present` storage/counting to
   the core model. Shell retains only pixel/GPUI painting. TUI retains only
   terminal measurement, `Pen` drawing, and cell hit translation.
7. **Register, do not branch.** Add terminal `MarkdownRows` after `TextRows` in
   `tui/src/rows.rs`'s `unified` builder for the same three extensions. Leave
   `split` unchanged. This makes `Host::layout`, `Host::wrap`, layout cycling,
   wrap cycling, and extension replacement work through the existing registry.

## Changes by layer

### Core

1. In `core/src/markdown.rs`, add the shared `Document` row model and borrowed
   row-view accessors. Reuse existing `Block`, `lay_out_tables`, `Tables`,
   `flow_table`, `FlowRow`, and `Wrapped`; do not reparse Markdown. Store flowed
   text/ranges once at reflow, not in render.
2. Move the policy in shell `MarkdownRows::reflow_tables`, `Rows::reflow`,
   `ruled`, and `text_of` into that model. Preserve the sparse `(row, grid)` and
   `(row, flow)` indexing and distinct-block budget cache so a diff without a
   table does no table work and a resize that crosses no budget is cheap.
3. Add a semantic `Furniture`/row-decoration query. Keep glyph choices
   configurable (the existing bullets and `TableGlyphs` are seams), and make
   continuation behavior explicit: bars repeat; a bullet is visible only on
   segment zero but reserves its slot on every segment; gutter numbers/signs are
   blank after segment zero.
4. Keep `core/Cargo.toml` untouched and dependency-free. Do not alter
   `Present`; the frontend adapters can delegate `claims`, `len`, `build`,
   `rows`, `width`, and `files` to the shared model while retaining unit-specific
   width calculation.

**Verify**: `cargo test -p gitten-core` passes, including new focused tests for
per-block budget selection, `Budget::At` table rows, the one-character squeeze
floor, bullet continuation furniture, fenced-vs-indented code, and
`rule_after` without an extra row.

### Desktop shell

1. In `shell/src/views/markdown.rs`, retain the public `MarkdownRows` adapter,
   GPUI `Metrics::for_font` behavior, `SharedString` conversion where required,
   and all rendering functions, but replace private block/table/flow/wrap state
   with the core `Document`.
2. Replace `MarkdownRows::furniture`, the per-line `Budget` match, and
   `MarkdownRows::ruled` with queries against core semantic furniture. Convert
   semantic slots to pixels once per distinct `Block`; preserve heading sizes,
   `Font::advance`, fixed row height, and the current 1px table rule.
3. Keep `shell/src/views/diff.rs`'s registry unchanged. Its existing order
   (`TextRows`, then Markdown specialist) and source-only `split` prove the
   extension seam still works.

**Verify**: `cargo test -p gitten-shell` passes with the existing Markdown
classification, table-flow, hit, selection, and registry tests unchanged or
tightened. A test must assert the shell and core model produce identical visual
row counts and `rule_after` positions for the committed slice.

### Terminal TUI

1. Add `tui/src/markdown.rs`. Its `MarkdownRows` wraps core `Document`, claims
   exactly `md`, `markdown`, and `mdx`, delegates the `Present` contract, and
   implements terminal `Rows` only for column budgeting, rendering, `hit`,
   `selectable`, and `report`.
2. Mirror the ordinary diff anatomy from `TextRows`: right-aligned old/new
   numbers resolved through `Theme::gutter_on(surface)`, sign, fixed Markdown
   furniture, then text. Row backgrounds/signs use existing `line_colors` and
   `runs::surfaces`; token pieces use `Theme::syntax_on(kind, surface)` and
   `Ink::styled`; marker/bar/rule colours come from `theme.markdown`. A heading
   is bold (terminal cells cannot change point size) while its level may affect
   indentation only if the shared furniture says so.
3. Generalize `tui/src/rows.rs`'s `text_run` internally so it can accept borrowed
   text/tokens/spans/kind/moved from either a prepared `Line` or a flowed core
   Markdown row. Keep the caller-owned `Vec<Run>` and call `runs`/
   `runs_selected`; no per-visible-row allocation.
4. For a quote or fenced block, draw the gutter and bar first, then call
   `Pen::scroll(Frame::shift)`, then paint the core-provided byte range. Active
   wrapping uses the core-selected budget after the bar/indent/bullet slots;
   wrap `off` keeps the whole logical line and pans only text. Fence language
   labels use the marker colour; fenced code body keeps syntax/intraline runs.
5. Implement `hit` with the exact same furniture width used for budgeting,
   `col_at`/`screen::cols`, the visual segment's core range, and `shift`.
   `selectable` returns the transformed text (or flowed table text) whose byte
   coordinates `hit` used, so copied text matches what was drawn.
6. In `tui/src/rows.rs`, change `Layouts::builtin`'s `unified` closure to return
   `TextRows` followed by Markdown `MarkdownRows`; do not add it to `split`.
   Export the module from `tui/src/lib.rs`.
7. If the table-rule spike succeeds, extend `Ink`/`sgr` in
   `tui/src/screen.rs` with an optional underline colour and apply it only for
   core `rule_after`. Assert fallback SGR includes normal underline as well as
   optional colour. If the spike fails the STOP condition below, do not replace
   it with an invented text row.

**Verify**: `cargo test -p gitten-tui` passes; `tui/src/diff.rs` and
`tui/src/main.rs` have no production diff after the change.

### App/config

No app change. Proven by `app/src/config.rs::apply_diff`, which already selects
`host.wrap` and assigns `host.layout`, and by `tui/src/diff.rs::with_layouts`,
`set_layout`, and `set_wrap`, which consume those values through registries.

## Test list

Create `tui/tests/fixtures/md.diff` as a small valid unified diff with `.md` and
`.rs` files and both removed/added sides. It must contain: ATX heading; the
literal line `    ./dev.sh diff        # rebuild on every save` (proves indented
`#` is not a heading); bullet with a wrapped continuation; quote; fenced code
with a long line and syntax tokens; strong/emphasis/link text; a thematic rule;
and a table whose wide cell squeezes to multiple sub-rows. Keep it small enough
to review in full and state in its test comment that it is the deterministic
slice standing in for ignored `fixtures/real/md.diff`.

Named tests and assertions:

- `core::markdown::tests::document_reflow_uses_each_blocks_budget` — distinct
  bullet/quote/heading/code budgets produce the expected ranges; headers and an
  intact table never get a column break.
- `core::markdown::tests::flowed_tables_keep_source_rows_and_mark_only_real_boundaries`
  — table squeezing returns more visual segments but the same logical-line
  count; `rule_after` is true only on the last segment between data rows and
  false on the separator/last row.
- `core::markdown::tests::indented_hash_stays_prose_beside_a_real_fence` — the
  exact indented fixture line is `Paragraph`, while fenced interior lines are
  `Code`.
- `tui::markdown::tests::builtin_registry_claims_markdown_only_in_unified` —
  `.md` goes to the specialist, `.rs` to `TextRows`, and `split` remains
  `SplitRows`; configured initial layout and cycling names remain `unified`,
  `split`.
- `tui::markdown::tests::markdown_frame_matches_the_cell_golden` — render the
  committed slice through `Diff`, `Screen`, and `Layouts::builtin` at a fixed
  width. Assert row text, right-aligned/blank continuation gutters, bullet/bar,
  bold heading, theme-derived inks, wide-cell alignment, squeezed table rows,
  and no fabricated numbered hairline row. Prefer explicit `Screen::row_text`
  and `ink` assertions over an opaque whole-screen snapshot.
- `tui::markdown::tests::fenced_code_scrolls_under_fixed_furniture` — with wrap
  off and nonzero `shift`, gutter/sign/code bar remain in the same columns while
  the token-styled text moves; with word wrap, the budget is net of the bar.
- `tui::markdown::tests::selection_hits_transformed_and_flowed_text` — drag
  across a stripped marker and a flowed table through `Diff::press`, `drag`,
  and `release`; `Diff::selection`/`copy_text` equals displayed text once, with
  no hidden Markdown marker and no duplicate wrapped logical row.
- Existing `tui/src/diff.rs` copy-on-select contract test remains passing. Add
  no special status path: integration coverage should show that the same
  non-empty selection reaches `tui/src/main.rs`'s existing mouse-up copy slot,
  whose `copied` helper still reports `copied N lines`.
- `tui::screen::tests::a_coloured_bottom_rule_preserves_cells_and_sgr_state`
  (only if coloured underline is selected) — rule decoration neither changes
  cell characters nor bleeds to the next run, and `Screen::flush` emits reset,
  underline, and underline-colour SGR.
- `shell::views::markdown` parity test — for the same fixture and budgets, core
  logical/visual row identities, flowed ranges, and `rule_after` markers match
  what the shell adapter consumes.

Headless fixture gate:

```sh
cargo test -p gitten-core
cargo test -p gitten-tui
cargo test -p gitten-shell
cargo fmt --check
cargo clippy -p gitten-core -p gitten-tui -p gitten-shell --all-targets
./check.sh
```

All must exit 0. None may launch a window, enter raw mode, switch to the
alternate screen, or require `fixtures/real/md.diff`.

Optional release guardrail, when the ignored corpus is available: run the dump
command from “Commands you will need” at 80, 120, and 150 columns. Record total
load, `prepare`, Markdown layout/table-flow contribution, visual rows, and
average frame. Expectations—not brittle pass/fail thresholds—are: one core
`prepare` near the documented 90.7 ms shape; one shared Markdown layout pass in
the documented 5.6 ms/78 ns-per-row class; table flow in the documented
1.0–1.4 ms range; and a visible-row-only frame near the current 12 µs source
frame, with no allocations after scratch buffers have warmed. STOP on a second
prepare/layout pass or frame cost that scales with total diff rows.

Done criteria:

- [ ] Only the in-scope files are modified (`git status --short`).
- [ ] `core/Cargo.toml` remains unchanged with empty `[dependencies]`.
- [ ] Both clients consume one core Markdown row/furniture/reflow model; grep
      finds no second `Budget::At` policy match or table-boundary rule in a
      frontend.
- [ ] TUI `unified` claims Markdown after `TextRows`; `split` stays source-only.
- [ ] Window pixel measurement and terminal cell measurement enter core only as
      supplied scalar budgets; no GPUI or `unicode-width` import enters core.
- [ ] Selection/copy, wrap/layout cycling, config opening choice, fixed-gutter
      horizontal pan, and table logical line counts are covered headlessly.
- [ ] Every verification command above exits 0.

## Stop conditions

Stop and escalate if any occurs:

- Sharing the model requires adding GPUI, crossterm, `unicode-width`, or any
  dependency to `core`, or putting render/input methods on core `Present`.
- The extraction makes the desktop presentation slower, visually worse, or
  unable to retain its 1px table hairline. The repository explicitly lets the
  desktop win over a shared seam.
- A core row model cannot expose prepared and flowed text to GPUI without a
  per-visible-row `String` allocation or full-row clone. Report the exact
  ownership mismatch (`Arc<str>`/`SharedString`/`FlowRow`) instead of hiding it
  in `render`.
- The only proposed table separator inserts an extra visual row or reuses a
  source `TableRule` as an inner boundary. That breaks row counts, gutter
  numbering, selection, and carets; do not ship it.
- Coloured underline is not supportable without materially expanding every
  `Cell`, regressing flush comparisons, or breaking existing underline SGR.
  Bring back a measured alternative or an explicit decision to omit inner rules
  in the terminal; do not invent glyph rows.
- Correct terminal table flow turns out to require solving the global
  character-versus-display-width `Wrap` seam. Report a minimized CJK case and
  split that prerequisite into its own plan rather than broadening this one.
- `Rows::hit` and `selectable` cannot use one byte-coordinate text after table
  flow, or a reflow changes copied content independently of displayed content.
- `fixtures/real/md.diff` is required for ordinary tests or the executor is
  tempted to commit the full corpus. Keep the deterministic small slice and the
  ignored benchmark separate.
- Any verification command launches a client, grabs a tty, needs network, or
  fails twice after one reasonable correction.

## Risks

- **Ownership and allocations (MED):** the shell currently converts flowed
  `String` to `SharedString` once. The shared model must preserve that one-time
  conversion and borrowed rendering; review allocations with particular care.
- **Two measurement systems (MED):** identical policy does not mean identical
  row breaks. The window derives columns from proportional pixel advance; the
  terminal measures furniture/hits/overflow in Unicode display cells while the
  current wrap algorithm still breaks by characters. Keep the unit adapter
  explicit and do not claim CJK wrap correctness this plan does not deliver.
- **Table bottom-rule portability (MED):** SGR underline colour is less universal
  than 24-bit foreground/background. Ordinary underline fallback keeps the
  boundary visible, but this needs a headless escape-code test and a quick dump
  inspection in at least one supported terminal before acceptance.
- **Selection coordinates (MED):** flowed table text contains embedded newlines
  whose `Break`s omit the separators. A mismatch among displayed ranges,
  `hit`, and `selectable` can silently copy the wrong bytes; the drag test is a
  release gate.
- **Registry drift (LOW):** adding a separate Markdown layout would make config
  and `s` behavior diverge. Review that both clients still expose exactly the
  same layout names and that specialist claim order is tested.
- **Performance (MED):** table flow is allowed at reflow/load, never per frame.
  Any scan of all Markdown rows from `render`, allocation of a run vector per
  row, or second prepare/layout pass violates the measured design even if the
  golden looks correct.
