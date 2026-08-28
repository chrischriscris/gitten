# Plan 020: Make terminal entry transactional so a failed start cannot leave raw mode on

> **Executor instructions**: Follow the plan step by step. Run every verification
> command and confirm the expected result before moving on. If a STOP condition
> fires, stop and report — do not improvise. Do not edit `plans/` in any way:
> the orchestrator owns the index. Do not push.
>
> **Drift check (run first)**: your worktree was bootstrapped for you. Verify
> `git -C "$WT" log --oneline` shows exactly two commits — `eb888e1` plus one
> `carry:` commit touching only `tui/src/main.rs`, `tui/src/term.rs`,
> `tui/src/scrollbar.rs` — and `git -C "$WT" status --short` is empty. The
> excerpts below are from that carried state. If anything differs, STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `eb888e1f3f3733b6f2020e2877c9d1fa68094f07`, 2026-08-28
  (working tree carried uncommitted changes in three tui files; they are commit 1
  of your branch)

## Why this matters

`Term::enter` enables raw mode **first** and then writes four escape sequences.
If any write or the flush fails (broken pipe, closed pty, a CI runner with no
stdout), `enter` returns `Err` **without constructing `Term`** — so its `Drop`
never runs — and the process exits with raw mode on and possibly the alternate
screen entered. The user's shell is left with no echo and no prompt: it looks
hung. `Term::guard` only covers panics, and `dev`'s belt (`stty sane`) only runs
when the process exits normally through the script.

## Current state

- `tui/src/term.rs` — the only module that touches crossterm. Constants:
  - `:122` `const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";`
  - `:123` `const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[?1000l";`
  - `:129` `const PASTE_ON: &[u8] = b"\x1b[?2004h";`
  - `:130` `const PASTE_OFF: &[u8] = b"\x1b[?2004l";`
- The bug, `tui/src/term.rs:152-165`:

```rust
pub fn enter(mouse: bool) -> io::Result<Self> {
    terminal::enable_raw_mode()?;
    let mut out = BufWriter::new(io::stdout());
    // Alternate screen first, then hide the cursor: the other order hides
    // the cursor on the *primary* screen and leaves it hidden there if the
    // next call fails.
    out.write_all(b"\x1b[?1049h\x1b[?25l")?;
    if mouse {
        out.write_all(MOUSE_ON)?;
    }
    out.write_all(PASTE_ON)?;
    out.flush()?;
    Ok(Self { out, entered: true })
}
```

- The restore that already exists and must be reused, `tui/src/term.rs:173-188`
  (`leave`): writes `MOUSE_OFF`, `PASTE_OFF`, `b"\x1b[?25h\x1b[?1049l"`, flushes,
  then `terminal::disable_raw_mode()` — every step attempted, errors ignored.
- The panic hook, `tui/src/term.rs:196-207` (`guard`): same restore sequence
  duplicated inline before calling the previous hook.
- The caller, `tui/src/main.rs:148-155` (carried tree): `Term::guard()` then
  `Term::enter(mouse)` matched with `Err(e) => { eprintln!; process::exit(1) }`.
  **No change is needed here** — the fix is entirely inside `enter`.

Conventions to match: doc comments explain *why* in prose; tests are plain
`#[test]` functions in the module's `mod tests`, no tty, no raw mode (see
`base64_is_the_one_in_the_rfc` for the style).

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test -q -p gitten-tui` (run with `CARGO_TARGET_DIR=/Users/chus/Projects/gitten.wt/tui/target`) | all pass |
| Lint | `cargo clippy -q -p gitten-tui --all-targets --locked -- -D warnings` | no warnings |
| Format | `cargo fmt --check` | clean |
| Scope | `git -C "$WT" status --short` | only `tui/src/term.rs` modified |

## Scope

**In scope**: `tui/src/term.rs` only.

**Out of scope** (do NOT touch, they look related):
- `tui/src/main.rs` — the caller is already correct once `enter` cleans up.
- `tui/src/scrollbar.rs` — carried WIP, not this plan's.
- Any byte sequence change. The sequences are correct; only the *transaction*
  around them is broken.

## Git workflow

- Branch: `advisor/020-transactional-terminal-entry` (already created, checked
  out in your worktree, with the carry commit on top of `eb888e1`).
- Commit per step. Message style: lowercase imperative, e.g.
  `tui: restore the terminal when entry fails halfway`.
- Do NOT push. Do NOT commit to any other branch or the main tree.

## Steps

### Step 1: Extract the two byte-level helpers

In `tui/src/term.rs`, add two private free functions and rewrite `leave`/`guard`
to use them. Byte sequences and their order are **exact** — they are pinned by
tests in step 4 and must not change what the terminal receives today:

```rust
/// The entry sequences, in the order `enter` has always sent them: alternate
/// screen, cursor hidden, then mouse tracking when asked for, then bracketed
/// paste.
fn bring_up(out: &mut impl Write, mouse: bool) -> io::Result<()> {
    out.write_all(b"\x1b[?1049h\x1b[?25l")?;
    if mouse {
        out.write_all(MOUSE_ON)?;
    }
    out.write_all(PASTE_ON)?;
    out.flush()
}

/// The restore sequences, every write attempted and every error ignored —
/// this runs while unwinding or after a partial failure, and a half-restored
/// terminal is the thing to avoid.
fn restore(out: &mut impl Write) {
    let _ = out.write_all(MOUSE_OFF);
    let _ = out.write_all(PASTE_OFF);
    let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
}
```

Rewrite `leave` to call `restore(&mut self.out)` after the `entered` check and
before `terminal::disable_raw_mode()`. Rewrite `guard` to call
`restore(&mut io::stdout())` (plus its existing `disable_raw_mode` and
`previous(info)` calls, in the current order). Byte output is identical to
today; this is a pure extraction.

**Verify**: `cargo build -q -p gitten-tui` → exit 0.

### Step 2: Make `enter` transactional

Rewrite `enter` so a failure after `enable_raw_mode` undoes everything:

```rust
pub fn enter(mouse: bool) -> io::Result<Self> {
    // Failing here has left nothing behind: raw mode was never on.
    terminal::enable_raw_mode()?;
    let mut out = BufWriter::new(io::stdout());
    match bring_up(&mut out, mouse) {
        Ok(()) => Ok(Self { out, entered: true }),
        // A partial bring-up is worse than none: put the terminal back
        // before reporting the failure, exactly as `Drop` would have.
        Err(e) => {
            restore(&mut out);
            let _ = terminal::disable_raw_mode();
            Err(e)
        }
    }
}
```

Keep the existing doc comment on `enter` and add a sentence: *entry is
transactional — a failed write restores what earlier writes did, so a caller
that sees `Err` owes the terminal nothing.*

**Verify**: `cargo build -q -p gitten-tui` → exit 0.

### Step 3: Tests

In `term.rs`'s existing `mod tests`, add (plain `#[test]`, no tty, no raw mode):

1. A `FailingWriter` implementing `std::io::Write` that records written bytes
   into a `Vec<u8>` and returns `Err` from a configurable call onward (count
   `write_all` calls; model the struct after any simple test double — the file
   has none yet, so a ~15-line struct with `impl Write` delegating to
   `Vec::extend_from_slice` is the expected shape).
2. `entry_sequences_are_the_ones_the_terminal_expects` — `bring_up` into a
   `Vec<u8>` cursor: with `mouse = false` the bytes are exactly
   `b"\x1b[?1049h\x1b[?25l\x1b[?2004h"`; with `mouse = true` the mouse-on
   sequence sits between the cursor-hide and paste-on bytes, exactly.
3. `restore_emits_every_off_sequence_in_order` — exact bytes
   `b"\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2004l\x1b[?25h\x1b[?1049l"`.
4. `a_failed_entry_restores_what_it_did` — a writer that fails on its third
   `write_all` (so alt-screen and cursor-hide landed, the mouse write fails):
   assert `bring_up`-through-`enter`'s error path returns the original error
   **and** the writer's log ends with the full `restore` byte sequence. Test
   the policy through a small private helper if that keeps `enter` itself
   tty-free — `enter` calls the real `terminal::enable_raw_mode` and must not
   be called in tests; test `bring_up`/`restore` plus one helper
   `fn enter_sequences(out: &mut impl Write, mouse: bool) -> io::Result<()>`
   that wraps `bring_up` with the restore-on-error policy, and have `enter`
   call *that*.
5. `restore_tolerates_a_dead_writer` — an always-failing writer: `restore`
   returns without panicking.

**Verify**: `cargo test -q -p gitten-tui term` → all new tests pass, none of the
existing ones regress.

### Step 4: Full gates

**Verify**:
- `cargo test -q -p gitten-tui` → all pass
- `cargo clippy -q -p gitten-tui --all-targets --locked -- -D warnings` → clean
- `cargo fmt --check` → clean
- `git -C "$WT" status --short` → only `tui/src/term.rs` (modified)

## Test plan

New tests are listed in step 3, all in `tui/src/term.rs`'s existing `mod tests`.
No existing test changes. The regression this plan fixes is pinned by test 4:
a write failure after raw mode must produce the restore bytes.

## Done criteria

- [ ] `cargo test -q -p gitten-tui` exits 0 including the four new tests
- [ ] `cargo clippy -q -p gitten-tui --all-targets --locked -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `git -C "$WT" diff eb888e1..HEAD --stat` shows only `tui/src/term.rs`
      beyond the carry commit
- [ ] `grep -c "1049h" tui/src/term.rs` finds the byte only inside `bring_up`
- [ ] `tui/src/main.rs` is untouched by this plan

## STOP conditions

Stop and report if:
- `Term`'s fields or the byte constants do not match the excerpts (drift).
- Making `enter` transactional appears to require touching `main.rs` or any
  file outside `tui/src/term.rs`.
- A test cannot be written without entering raw mode or a real tty.

## Maintenance notes

- `restore` is now the single owner of the OFF sequence; `guard` and `leave`
  share it. A future mode added to `enter` (e.g. kitty keyboard protocol) must
  add its OFF half to `restore`, or the panic path will not undo it — the byte
  tests make the pairing visible.
- Reviewer focus: the error path in `enter` must attempt `disable_raw_mode`
  even though `restore`'s writes failed — that call is the half that actually
  un-hangs the shell.
