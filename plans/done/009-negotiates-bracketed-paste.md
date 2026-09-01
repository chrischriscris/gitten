# Plan 009: Negotiate bracketed paste so pasting is safe in the terminal

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 2dfcb82..HEAD -- tui/src/term.rs tui/src/main.rs`
> If these files changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `2dfcb82`, 2026-08-26

## Why this matters

Pasting text into `gitten-tui` today types it as keystrokes: a paragraph with a
`q` in it quits the app mid-paste, and every pasted character costs one keymap
lookup and one full frame while the terminal queues thousands of them. The
standard mechanism — **bracketed paste**, mode 2004 — makes the emulator wrap
pasted bytes in sentinels so the client receives one `Event::Paste` instead of
N keys. This app has no text input anywhere (it is read-only), so the entire
correct behavior for a paste event is to drop it — and the code already drops
it (`translate_event`'s `_ => None`). What is missing is *negotiating* mode
2004, symmetrically in enter, leave, and the panic hook, exactly like the mouse
modes beside which it belongs.

## Current state

- `tui/src/term.rs` — the terminal boundary; a keypress becomes `core`'s idea
  of a key here and nowhere inland.
  - Mode constants at :114–115:
    ```rust
    const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
    const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[?1000l";
    ```
  - `enter`, :133–145 — raw mode first, then fallible writes with `?`:
    ```rust
    pub fn enter(mouse: bool) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = BufWriter::new(io::stdout());
        // Alternate screen first, then hide the cursor: ...
        out.write_all(b"\x1b[?1049h\x1b[?25l")?;
        if mouse {
            out.write_all(MOUSE_ON)?;
        }
        out.flush()?;
        Ok(Self { out, entered: true })
    }
    ```
  - `leave`, :153–166 — best-effort reverse order; drops happen even when an
    earlier step failed ("every step is attempted").
  - The panic hook, :174–184 (`Term::guard`) — writes `MOUSE_OFF` +
    cursor/alt-screen restore + flush before chaining the previous hook.
  - `translate_event`, :293–300:
    ```rust
    match event {
        Event::Resize(w, h) => Some(Input::Resize(w as usize, h as usize)),
        Event::Key(k) if k.kind != KeyEventKind::Release => translate(k),
        Event::Mouse(m) => mouse(m),
        _ => None,
    }
    ```
    With crossterm, a paste arrives as `Event::Paste(_)` **only** once mode 2004
    is on; without negotiation there is no such event and the emulator streams
    plain keys through `Event::Key`.
  - `poll`'s doc comment, :234–238, already claims "a bracketed paste … is
    skipped rather than surfaced". It describes what the code *wants* to be
    true; only the mode bits are missing to make it true.
- `tui/src/main.rs`: the loop dispatches on the three `Input` variants at
  :440–448; no changes needed there.
- Conventions: two dependencies total in this crate by design (crossterm and
  libc); everything terminal-mechanism-shaped lives in `term.rs`; doc comments
  state the trade in prose. Match that voice.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Crate tests | `cargo test -p gitten-tui` | all pass |
| Full gate | `./check.sh` | exit 0 |
| Structural check | `grep -n '?2004' tui/src/term.rs` | exactly 3 hits after the change (ON in enter area, OFF in leave area, OFF in guard) |

## Scope

**In scope** (the only files you should modify):
- `tui/src/term.rs`

**Out of scope** (do NOT touch):
- `tui/src/main.rs` — no new `Input` variants, no burst-collapse of queued
  events (deferred; see Maintenance notes).
- Any keymap/press logic or `Input`'s shape.
- Mouse-mode constants beyond leaving them intact.

## Git workflow

- Branch: `advisor/009-negotiates-bracketed-paste`
- Commit message style: sentence-case imperative like `Stop a revspec from
  being read as a git option`. Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Add the constants

Beside `MOUSE_ON`/`MOUSE_OFF` (:114–115):

```rust
/// Bracketed paste (2004). On, the emulator wraps a paste in ESC[200~ /
/// ESC[201~ sentinels and crossterm hands over one `Event::Paste` instead of
/// streaming N key events — see `translate_event`. Off is the default in
/// every terminal, so old ones ignore both sequences silently.
const PASTE_ON: &[u8] = b"\x1b[?2004h";
const PASTE_OFF: &[u8] = b"\x1b[?2004l";
```

**Verify**: `cargo build -p gitten-tui` → exit 0.

### Step 2: Turn it on in `enter`

After the mouse block, before `flush()`:

```rust
out.write_all(PASTE_ON)?;
```

Add one sentence to `enter`'s existing doc comment naming why: without it a
paste is typed into the keymap character by character.

**Verify**: `cargo build -p gitten-tui` → exit 0.

### Step 3: Take it off everywhere restoration happens

1. In `leave` (:153–166): write `PASTE_OFF` alongside `MOUSE_OFF`, unconditionally
   (same rationale the comment above those lines already gives for the mouse).
2. In the panic hook body in `Term::guard` (:176–183): add
   `let _ = out.write_all(PASTE_OFF);` next to the `MOUSE_OFF` write. Note the
   hook binds `io::stdout()` itself (`let mut out = io::stdout();`), not the
   buffered writer — keep using what the hook uses.

Order note: restoration must undo everything acquisition did even when pieces
fail, per the module comment at :150–152 — do not introduce early returns.

**Verify**: `grep -n '?2004' tui/src/term.rs` → exactly 3 hits
(`enter`, `leave`, `guard`).

### Step 4: Make the docs true

Update the `poll` doc comment (:234–238) so the bracketed-paste sentence states
the mechanism honestly, e.g.: "*Because* bracketed paste is negotiated in
[`Term::enter`], an emulator delivers a paste as one [`Event`] variant that
`translate_event` drops — `q` inside a pasted paragraph cannot quit anything."

Also extend `translate_event`'s `_ => None` arm with a short comment naming
`Event::Paste` explicitly as deliberate-and-dropped because the app takes no
text input, so a future reader doesn't wire it up by accident.

**Verify**: `cargo test -p gitten-tui` → all pass.

### Step 5: Full gate

**Verify**: `./check.sh` → exit 0.

## Test plan

A real paste needs a pty and an emulator; there is none in headless CI, and the
module's own tests confirm that honesty. Machine-checked coverage:

- Structural: the grep in Step 3 proves all three restoration sites cover the
  mode (a reviewer should eyeball symmetry ON→OFF between enter and leave/guard).
- Regression risk lives in `cargo test -p gitten-tui` staying green: nothing
  about existing parsing/key translation may change.

Manual checklist for the human reviewer (documented here because it cannot be
automated): run `./dev tui commits`, open your shell history, paste a line
containing `q` — expected: nothing happens, then normal keys work. Paste
without quotes at your own risk.

## Done criteria

All must hold:

- [ ] `grep -c '2004' tui/src/term.rs` ≥ 5 (two consts + three use sites)
- [ ] `cargo test -p gitten-tui` exits 0
- [ ] `./check.sh` exits 0
- [ ] No files outside the in-scope list modified (`git status --short`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `tui/src/term.rs` diverges from the excerpts (e.g. `guard` no longer installs
  its hook before raw mode, or `leave` changed shape) — say what you found.
- You find a bracketed-paste test dependency or pty harness already in the tree
  (then mirror it instead of the structural assertion).
- Any existing test asserts on the exact byte sequence `Term::enter` writes
  (update expectations would be required — report first, since changing tests
  is outside this plan's spirit).

## Maintenance notes

- The deferred follow-up: collapsing burst input (drain up to N queued events
  per iteration before drawing). Not done here to keep the plan S; note that
  once a commit-message prompt exists (roadmap A#4), `Event::Paste` stops being
  droppable — the tui will want the payload routed into the input seam, making
  this file's `None` arm a site to revisit deliberately.
- Reviewer focus: PASTE_OFF presence in **both** leave and guard; absence of
  any change to `MOUSE_*` ordering (alternate-screen/cursor ordering has a
  documented reason).
