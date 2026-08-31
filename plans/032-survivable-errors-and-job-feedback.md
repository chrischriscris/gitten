# Plan 032: Git's own words survive — errors are readable, dismissable, copyable; jobs show time

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 038d0ad..HEAD -- shell/src/main.rs shell/src/chrome.rs core/src/command.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

> **Base — read before your first command**: create your branch from
> `origin/full/full` (`038d0ad`), NOT from whatever HEAD your worktree starts
> on:
>
> ```sh
> git switch -c <the branch named under "Git workflow"> origin/full/full
> ```
>
> Line numbers in this plan were refreshed against `038d0ad`. Where one is off
> by a few lines, **match on the quoted content** — every excerpt is verbatim
> from that commit.
>
> **Build cost**: this workspace builds GPUI. Export a shared target dir first
> so you are not doing a cold build of the whole tree:
> `export CARGO_TARGET_DIR=/tmp/gitten-pass6-target`. Cargo locks it, so if
> another executor is mid-build your first command may wait — that is expected,
> not a hang.
>
> **Palette note**: `chrome.raised` and `chrome.keycap` do **not** exist on this
> base — they live in an uncommitted design pass in the author's working tree.
> Do not add them and do not reference them. Any step that would need them is
> marked SKIP-AND-REPORT.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none (plan 031 touches the same message-band lines in
  `shell/src/main.rs`; if 031 landed first, its `Notice` enum is the base to
  build on — reconcile, don't duplicate)
- **Category**: bug (UX) — error/feedback surface
- **Planned at**: commit `038d0ad` (`origin/full/full`), 2026-08-31

## Why this matters

gitten shells out to the `git` binary on purpose, so git's stderr is the
product's error message — and today it is the least readable text in the app.
Every failure is formatted as `git {args}: {stderr}` (`git/src/lib.rs:90-93`),
stored whole, and rendered as a **single truncated line** in a 26px status
band (`shell/src/main.rs:4501`). A rejected push is 5–8 lines of hints; the
user sees one clipped fragment prefixed by an argv they never typed. The
error is also **sticky in the wrong way**: a resolved keypress clears
`notice` but not `error` (`main.rs:3524-3527`), `esc` does not clear it, and
while it stands the key hints — the footer's only discoverability surface —
are blanked (`main.rs:4477-4478`). It cannot be scrolled, expanded, copied or
dismissed; the user re-runs the command in a terminal to learn why gitten
refused.

Two smaller feedback gaps ride along because they live in the same band:
a running job is the static string `running {name}` with no elapsed time
(`main.rs:2716`) — a push blocked on a slow remote looks identical at second
1 and second 30 — and the "loading" state is stated twice at once (a header
word and a band line from the same cell). And `y` (copy) gives zero feedback
on success or on empty (`copy_selection`, `main.rs:3432+`), violating the
file's own stated principle ("a key that does nothing and a key that is not
bound look identical", `main.rs:3546-3548`).

After this plan: the band shows the error's first meaningful line and stays
until dismissed with `esc`; a bound key opens the full text in an overlay
panel (word-wrapped, copyable with `y`); running jobs show elapsed seconds;
"loading" has one home; and every copy answers with a notice.

## Current state

- `git/src/lib.rs:90-93` — the error string's birth:

```rust
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args.join(" "), err.trim()));
    }
```

Do **not** change this format (out of scope — the string shape is tested and
shared); the shell parses it for display instead.

- `shell/src/main.rs` fields: `running: Option<String>` (~1096),
  `error: Option<SharedString>` (~1116), `notice: Option<String>` (~1122).
- `drain_jobs` (~2710-2740): `JobEvent::Started` sets
  `self.running = Some(format!("running {name}"))` and clears `error`;
  `Finished(Err)` sets `self.error`.
- The band render (~4468-4505): builds
  `error → notice → running` precedence, blanks hints when any message
  stands, and draws the message as one `.truncate()`d div.
- `back()` (~3125-3127): closes help, does not clear `error`.
- `copy_selection` (~3432+): writes the clipboard, returns silently; the
  empty case is a silent no-op.
- The help overlay (`shell/src/help.rs`) is the exemplar for a deferred,
  occluding, centered panel — reuse its shape (`deferred(...)`, `.occlude()`,
  `bg`, border, `rounded(px(4.))`, `p(px(PAD))`).
- Keys are data: `core/src/command.rs` — `Keymap::builtin()` binds names;
  `Commands::builtin()` registers `(name, doc, hint)`. `bind` returns an
  error on a collision (there are tests asserting this), so a new binding
  that collides fails loudly at test time.
- The diff-pane header draws an accent `"loading"` word (~4226-4229) while
  the band shows `"loading diff"` from the same `self.loading` cell (~4272).

Repo conventions: keys/commands are registered in `core` and dispatched by
name in `run_command` (`main.rs:2967`, one big match). Doc comments argue for
choices. Commit style `crate: lowercase sentence`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| App tests | `cargo test -q -p gitten-app` | exit 0 |
| Everything | `./dev check` | exit 0, no `✗` |

Never launch `./dev desktop` (repo rule: no windows unasked).

## Scope

**In scope**:
- `shell/src/main.rs`
- `shell/src/chrome.rs` (only if you factor the panel or band helpers there)
- `core/src/command.rs` (one new command + binding + hint)
- A new `shell/src/message.rs` if the overlay earns its own file (optional)

**Out of scope**:
- `git/src/lib.rs` — the error string format stays.
- `app/src/jobs.rs` — no progress streaming, no cancellation in this plan
  (deferred; see Maintenance notes).
- `tui/` — the terminal has its own message line.
- Plan 031's armed-question ink (if unlanded, don't absorb it here).

## Git workflow

- Branch: `advisor/ui-032-survivable-errors`
- Commit per step; style: `shell,core: an error is a panel, the band is its first line`
- No push/PR unless instructed.

## Steps

### Step 1: Structure the error

In `shell/src/main.rs`, replace `error: Option<SharedString>` with a small
struct:

```rust
/// A refusal, kept whole. The band shows `summary`; the overlay shows `full`.
struct GitError {
    /// The first line of git's own words, argv prefix stripped.
    summary: SharedString,
    /// Everything git said, verbatim — the argv prefix included, because
    /// "which command" is part of the answer when the text is being read
    /// rather than glanced at.
    full: SharedString,
}
```

Derive `summary` where the error arrives (in `drain_jobs` and any other
`self.error = ...` site — grep `self.error`): strip the leading
`git <args>: ` prefix if present (split on the first `: ` only when the
string starts with `"git "`), then take the first non-empty line. Keep
`full` as received. Update every read of `self.error` (the band render, the
`Started` clear).

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 2: `esc` dismisses; errors stop outliving their moment

- In `back()` (~3125): before the existing ladder, if `self.error.is_some()`,
  clear it and return (one step of the ladder, like help closing).
- Do NOT make ordinary keypresses clear it (deliberate: an error should
  survive a scroll), but the band must say how to leave: render the summary
  followed by a faint suffix naming the live keys, e.g.
  `· esc dismiss · <key> full text` — resolve the second key via
  `host.keys.live_keys_for("message.show", &self.modes)` (see Step 3), and
  omit the suffix piece when no live key exists (the help overlay's rule:
  "no live key, no hint", `shell/src/help.rs:72-87`).

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 3: The full-text overlay

- In `core/src/command.rs`: register `message.show` in `Commands::builtin()`
  with doc `"show the full text of the last message"` and hint `None`; bind
  it in `Keymap::builtin()` under `GLOBAL` to `` "`" `` (backtick — unused by
  lazygit's defaults and unclaimed in the shipped map; `bind` will error at
  test time if that has changed — if it errors, STOP and report rather than
  picking another key).
- In `shell/src/main.rs` `run_command`: `"message.show"` toggles a
  `show_message: bool` (only meaningful when `self.error` is some; a no-op
  otherwise, and the band only advertises it when an error stands).
- Render: when `show_message && self.error.is_some()`, draw a panel modeled
  on `help::overlay` — `deferred`, `.occlude()`, centered, `max_w` ~720px,
  `max_h_full`, the full text word-wrapped (a `div` with the text and no
  `whitespace_nowrap` wraps by default), error-ink heading line, dim body.
  `esc` closes it first (extend `back()`'s ladder), and while it is up, `y`
  copies `full` to the clipboard (route in `copy_selection`: if the panel is
  up, copy the error and `set_notice("copied")`, return).
- Mode handling: push a `"message"` mode like help pushes `"help"` in
  `sync_modes` (~1288) so the status badge tells the truth.

**Verify**: `cargo test -q -p gitten-core` → exit 0 (the new bind holds —
no collision). `cargo test -q -p gitten-shell` → exit 0.

### Step 4: Elapsed time on running jobs; one home for "loading"

- Change `running: Option<String>` to `Option<(String, std::time::Instant)>`.
  In the band render, append elapsed once it passes 1s:
  `running push · 4s` (whole seconds; no spinner — quiet chrome).
- GPUI draws nothing at rest, so the seconds won't tick without a nudge:
  when a job starts, spawn a repeating notifier —
  `cx.spawn(async move |this, cx| { loop { Timer::after(Duration::from_secs(1)).await; ... this.update(cx, |_, cx| cx.notify()) ... } })`
  — that exits when `running` is `None`. Look at existing `cx.spawn` uses in
  `main.rs` for the idiom and error handling; if none exists in the crate,
  STOP and report (the reactive-tick mechanism deserves a human look).
- Remove the accent `"loading"` word from the diff pane header (~4226-4229);
  the band's `"loading diff"` (~4272) is the one home. Keep the header's
  other right-edge furniture.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.
`grep -n '"loading"' shell/src/main.rs` → only the band's remains.

### Step 5: Copy acknowledges

In `copy_selection` (~3432): after each successful clipboard write, call
`self.set_notice(...)` — `"copied"` for a text selection, `"copied <what>"`
where the branch already knows what it copied (a sha, a path); when the
selection and cursor text are both empty, `self.set_notice("nothing to copy")`.
Notices already clear on the next resolved key (`main.rs:3527`) — transient
by design, no new plumbing.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 6: Full gate

**Verify**: `./dev check` → exit 0, no `✗`.

## Test plan

- New shell test: the error summary derivation — given
  `"git push origin main: error: failed to push some refs\nhint: ..."`,
  summary is `"error: failed to push some refs"` and `full` is the whole
  string. Cover: no `git ` prefix, empty stderr, single line.
- New core test in `command.rs` tests: `message.show` resolves from GLOBAL
  and `live_keys_for("message.show", ...)` returns the backtick (model on
  the existing `live_keys_for` tests, ~2110+).
- New/extended shell test: `back()` clears the overlay first, then the
  error, then falls through to its existing ladder (assert the order).
- Existing tests must keep passing untouched except where `error`'s type
  changed.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] `grep -n "message.show" core/src/command.rs` shows the command and the
      binding
- [ ] `grep -n "esc" shell/src/main.rs` — `back()` clears error/overlay
      (verify by the new test, not the grep alone)
- [ ] `grep -cn "set_notice" shell/src/main.rs` increased (copy acks)
- [ ] `grep -n '"loading"' shell/src/main.rs` → one home
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

- The backtick binding collides (`bind` errors) — report; the key choice is
  a maintainer call.
- No existing `cx.spawn`/timer idiom exists in `shell/` to model the
  elapsed-seconds tick on.
- Plan 031 landed with a `Notice` enum that conflicts with the `GitError`
  struct's band precedence — reconcile by reading 031's diff first; if the
  precedence rules genuinely conflict, report.
- The `self.error` type change fans out beyond `shell/src/main.rs`.

## Maintenance notes

- Deferred deliberately: streaming git's stderr as it runs and job
  cancellation (both need `app/src/jobs.rs` to grow an `Event::Output` /
  kill handle — an L change), and a persistent command log (see the
  direction findings in plans/README.md pass-3 notes; this plan's `GitError`
  retention is the seed of it).
- Reviewers should scrutinize: the summary-derivation split (brittle string
  parsing — the tests pin it), and that the overlay's `occlude` really does
  keep keys from reaching panes (same mechanism as help; plan 033 fixes
  help's own keyboard passthrough).
