# Plan 045: Clicking a row selects it

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update this plan's row in
> `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `grep -n "fn row_bar" shell/src/views/diff.rs`
> must hit (the design pass is on your base). Line refs were taken at
> `00842dc` + the staged design pass; where one is off by a few lines, match
> on the quoted content. On a structural mismatch (a symbol named here does
> not exist), STOP.
>
> **Build cost**: `export CARGO_TARGET_DIR=/tmp/gitten-target` before the
> first cargo command. Never launch `./dev desktop` or `./dev tui`.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug (UX) — the mouse can aim at nothing in the sidebar

## Why this matters

Clicking a row in any sidebar pane focuses the pane and does **not** move the
cursor to the clicked row. The only mouse verb the sidebar has is
`capture_any_mouse_down → focus_named` at the *section* level
(`shell/src/main.rs:4280-4282` and `:4325-4327`):

```rust
.capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
    this.focus_named(name, cx);
}))
```

So a mouse user sees a list of files, clicks one, and the selection stays
where the keyboard left it — the single biggest mouse gap in the app. The
diff pane already does this right (`click_row` in `shell/src/views/diff.rs`
moves the cursor to the clicked row), so the sidebar is an inconsistency, not
a philosophy.

After this plan: a click in files, branches, commits or stashes focuses the
pane **and** puts the cursor on the clicked row, with exactly the side
effects a keyboard cursor move has (armed destructive questions disarm,
notices clear). Section labels (`STAGED`, `LOCAL`, …) stay unselectable —
clicking one focuses the pane and nothing else, same as today.

## Current state

- Each sidebar view renders rows through `uniform_list` with a row-builder
  closure; the closure knows each row's index. Row frames come from
  `chrome::list_row` (`shell/src/chrome.rs:57`), which returns a plain `Div`
  with no id and no click handler.
- The views already have a single keyboard cursor-move path (the `cursor`
  field plus the move verbs that clamp, skip section labels, and disarm; see
  the comments at `shell/src/main.rs:2093` — "any cursor move, wheel or
  refresh disarms"). Branches and files additionally skip heading rows when
  moving (`shell/src/views/files.rs` `cursor` docs, `branches.rs:391-403`
  region).
- GPUI rule (CLAUDE.md): interactivity requires identity — `.id()` before
  any `on_click`/`on_mouse_down`, and an element's identity is its *path*,
  so per-row ids must be unique within the pane (`("row", index)` is the
  idiom) and the pane wrapper already carries its own id.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Everything headless | `./dev check` | exit 0 |
| One frame, no window | `./dev dump commits . 2>/dev/null \| head -20` | rows on stdout |

## Scope

**In scope**: `shell/src/views/files.rs`, `branches.rs`, `commits.rs`,
`stashes.rs`; `shell/src/main.rs` only if the wiring needs a listener the
views cannot own; `shell/src/chrome.rs` only if `list_row` grows an optional
id/handler parameter shared by all four panes (preferred over four copies).

**Out of scope**: the diff pane (already correct); double-click semantics;
context menus (plan 050); hover styling (plan 053); the section-level
`capture_any_mouse_down` (leave it — capture runs before bubble, so the
focus-then-select order is free).

## Git workflow

- Branch: `advisor/ui-045-click-selects-the-row`
- Commit style: `shell: a click lands the cursor on the row it hit`
- No push, no PR, unless the operator instructed it.

## Steps

### Step 1: One shared verb, not four

Add a `select_row(&mut self, index: usize)` (name to taste, match the house
voice) to each view — or better, confirm each view already has the internal
"cursor to absolute row, with side effects" function its keyboard verbs call,
and expose that. It must: clamp, refuse section-label rows (snap to the
nearest selectable row below, the same rule the keyboard uses), disarm any
armed question, and `cx.notify()`.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 2: Wire the click

In each view's row builder: give the row `.id(("row", index))` and
`.on_mouse_down(MouseButton::Left, cx.listener(...))` (mouse-down, not click:
selection should land before any drag or double-click story starts, and the
diff's `press` uses mouse-down for the same reason). The handler calls the
Step 1 verb. Section-label rows get no handler.

If threading `cx.listener` into the shared `chrome::list_row` is awkward,
attach the handler in the view around the returned `Div` — do not fork
`list_row` into four variants.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.
`./dev dump commits . 2>/dev/null | head -5` still renders.

### Step 3: Tests

Sentence-named, per house style, e.g.
`a_click_moves_the_cursor_to_the_row_it_hit` and
`a_click_on_a_section_label_selects_nothing_and_a_click_disarms_a_question`.
Exercise the Step 1 verb directly (arm a discard, "click" another row,
assert disarmed and cursor moved) — the GPUI event plumbing needs no test,
the verb does.

**Verify**: `./dev check` → exit 0.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] Every one of files/branches/commits/stashes has a row-level mouse-down
      path reaching the same cursor-move code its keyboard verbs use
      (grep each view for the Step 1 verb; it has ≥2 callers: keys + mouse)
- [ ] Section labels remain unselectable
- [ ] An armed question disarms on click (test proves it)
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/high-priority/README.md` row updated

## STOP conditions

- A view has **no** single internal cursor-move-with-side-effects function
  and its keyboard verbs write `self.cursor` directly in more than two
  places — report the shape before unifying it; that refactor may be bigger
  than this plan.
- Adding `.id()` to rows breaks `uniform_list` row identity in a way that
  shows up as hover/click state shared between panes (the "element identity
  is its path" trap) and a unique wrapper id does not fix it on first try.
