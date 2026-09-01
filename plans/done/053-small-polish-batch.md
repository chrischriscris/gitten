# Plan 053: Small polish batch — eight independent items

> **Executor instructions**: Each item is independent; do them in order,
> one commit each, and a failed item is skipped-and-reported, never
> improvised around. Run the named verification after each item. When done,
> update this plan's row in `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `grep -n "fn row_bar" shell/src/views/diff.rs`
> must hit (the design pass is on your base). Line refs were taken at
> `00842dc` + the staged design pass; match on quoted content where a ref
> drifted; STOP on a structural mismatch.
>
> **Dedupe fence**: plan 059 (nits bundle) owns: prompt selection colour,
> help backdrop scrim, the dels-glyph spelling, suppressing the FILES zero
> count, chrome.rs's stale pane-number doc, right-edge furniture vs the
> scrollbar track. Do not touch those here. **Item 5 below edits the same
> pane-header call sites as 059's zero-count item** — if 059 has landed,
> rebase item 5 on its result; if not, keep the edits minimal so the merge
> is clean.
>
> **Build cost**: `export CARGO_TARGET_DIR=/tmp/gitten-target`. Never launch
> `./dev desktop` or `./dev tui`.

## Status

- **Priority**: P3 (each item S)
- **Effort**: S ×8
- **Risk**: LOW
- **Depends on**: item 7 references plan 047's landed comment rewrite;
  everything else standalone
- **Category**: polish

## Git workflow

- Branch: `advisor/ui-053-polish`
- One commit per item, message naming the item, e.g.
  `shell: rows that take a click say so on hover`
- No push, no PR, unless the operator instructed it.

## Items

### 1. Hover says "clickable"

Sidebar rows take a click (plan 045) but show nothing on hover — `hover(`
exists only in `controls.rs`. Give the sidebar row frame (best: one change
in `chrome::list_row`, `shell/src/chrome.rs:57`) a hover background of
`c.raised` — the palette's "lifted one step" surface — applied only when the
row is not `current` (the selection tint outranks a hover), plus
`cursor_pointer`. Section labels get neither.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. Note: `hover` needs
identity — if `list_row`'s `Div` has no `.id()`, apply the hover in the
views where plan 045 added ids, not in `chrome.rs`, and say so in the
commit.

### 2. Commit ages don't go stale

Ages are banded once at load ("computed once per load", the comment near
`shell/src/views/commits.rs:566-574`) — an hour later "5m" is a lie. Keep
the banding function (`commits.rs:707`) pure; store each row's timestamp at
flatten, and re-band on a coarse shell-side timer: a 30-second
`cx.spawn`-style interval on the commits view that only `cx.notify()`s when
at least one visible band would change (compare the coarsest boundary, not
per-row strings — no per-frame work, per the render-path rule).

**Verify**: `cargo test -q -p gitten-shell` → exit 0; a test that a
timestamp crossing a band boundary re-bands
(`an_age_crosses_a_band_when_time_passes` — drive the pure function with two
instants).

### 3. A position indicator by default

`[view] scrollbar` defaults off, so a first launch shows a 714k-row diff
with no sense of where or how much. Change the default to on in
`app/src/config.rs` (and the `./dev config` template's comment — the knob
stays; this flips only the default). The bars are the quiet 8px overlay
style from `views/mod.rs:16-21`, already gitten-styled — defaulting them on
costs no chrome.

**Verify**: `cargo test -q -p gitten-app` → exit 0 (update the default's
test); `./dev config | grep scrollbar` shows the new default with its
comment.

### 4. Load logging stops going to stderr unasked

Every refresh prints to stderr (`files.rs:295`, `branches.rs:339`,
`stashes.rs:74`, `commits.rs:585`, `diff.rs:1509`, `diff.rs:1694`) — noise
in a GUI app's console and cost on every refresh wave. Gate each behind
`stats::enabled()` (`shell/src/stats.rs:50` — the `GITTEN_STATS` check the
overlay already uses), so the readout users get the lines and nobody else
does.

**Verify**: `cargo test -q -p gitten-shell` → exit 0;
`GITTEN_STATS=0 ./dev dump commits . 2>/dev/null 1>/dev/null` — wrap:
`GITTEN_STATS=0 ./dev dump commits . 1>/dev/null` prints no `flatten` lines
to stderr (the dump's own timing lines are stats-gated too; if dump *needs*
them, key dump's path on `stats::enabled()` defaulting on in `./dev dump` —
check how `./dev dump` sets the env before changing behaviour).

### 5. BRANCHES and STASH headers count like FILES

The FILES pane header carries its count; branches and stashes pass
`count: None` (`main.rs:4289` and the `STACK_FOOT` twin at `:4334`) even
though the in-list `LOCAL 16` counts exist. Pass each pane's total through
the same `Option<SharedString>` the files header uses, sourced from the view
(`rows()` minus headings — use whatever accessor the in-list labels read, so
the two numbers cannot disagree).

**Verify**: `cargo test -q -p gitten-shell` → exit 0. **Cross-note**: plan
059's item suppresses FILES' zero — match its rule (a zero count is dropped,
not printed) so all three headers agree.

### 6. The input band joins the rhythm

The prompt input's height is a literal `px(34.0)` (`shell/src/input.rs:706`)
against a chrome of named 26px strips (`HEADER_H`, `STATUS_H`,
`chrome.rs:27-31`). Name it in `input.rs` (`INPUT_H`), set it to a value on
the app's rhythm — `STATUS_H + 8.0` if the field needs the air, with the
doc comment saying *why* it is taller than the strips it sits between (a
text field is a target, not a label) — and read it wherever 34 was inlined.
Plan 058 (metric unification) explicitly leaves height constants alone, so
this does not collide.

**Verify**: `cargo test -q -p gitten-shell` → exit 0;
`grep -n "34.0" shell/src/input.rs` → no hits.

### 7. Two stale doc comments

- `shell/src/views/diff.rs:184-193`: `RowState.focused`/`armed` still carry
  `#[allow(dead_code)]` and "no drawing reads it yet" — `row_bar` and the
  armed tint read both. Drop the allows (the compiler confirms) and rewrite
  the comments to say what reads them.
- `shell/src/views/branches.rs:60-63`: "each branch keeps one colour across
  the app" — if plan 047 landed, confirm it rewrote this; if 047 has not
  landed, leave it and report (047 owns that rewrite; do not half-fix).

**Verify**: `cargo build -q -p gitten-shell` warns of nothing new; grep
confirms the dead_code allows are gone from that block.

### 8. The minimum window holds its floors

`window_min_size` is 560×320 (`main.rs:5458`), but five stacked sections'
floors (5 × `SECTION_MIN_H` = 5 × ~70px) plus title (32), status (26) and
borders exceed 320 — sections silently clip at the declared minimum. Raise
the minimum height to the computed sum (a named expression beside
`SECTION_MIN_H`, not a literal — the arithmetic is the documentation), on
the order of 460px. Keep 560 wide.

**Verify**: `cargo test -q -p gitten-shell` → exit 0; add
`the_minimum_window_holds_every_sections_floor` computing the same sum the
constant does and asserting `min_h >= sum`.

## Done criteria

- [ ] `./dev check` exits 0 after the final item
- [ ] Eight commits (or fewer plus explicit skip reports)
- [ ] No touched site belongs to plan 059's list (the dedupe fence)
- [ ] `plans/high-priority/README.md` row updated with per-item outcomes

## STOP conditions

- Any item's "small" fix wants a second file the item does not name —
  report it instead of growing the item; this plan's value is that every
  item is genuinely small.
- Item 3: if the default flip makes any headless test or `./dev dump`
  render scrollbars into the dumped frame, report — the dump's frame is
  compared in tests and must stay stable.
