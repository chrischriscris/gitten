# Plan 005: A panic or oversized line in one web request cannot kill the server

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result. If a "STOP conditions"
> item occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 3a8b347..HEAD -- web/src/http.rs web/src/lib.rs`
> If either changed, compare the excerpts against the live code; on a mismatch,
> STOP.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug / security (availability)
- **Planned at**: commit `3a8b347`, 2026-08-24

## Why this matters

Two robustness holes in the hand-rolled web server:

1. **The request handler runs on the main thread with no panic boundary.**
   Connection threads are deliberately panic-isolated (a malformed request
   should not take the server down), but the handler that does the actual
   routing runs on the thread that called `serve` — which is `main`. Any panic
   reachable from routing (a third-party `Wrap`/`Highlighter` bug, an index
   slip) unwinds out of `serve` and ends the process, so every browser tab just
   stops loading with no error surfaced. Worse, the doc mutex is taken with
   `.expect("no request panics while holding the doc")`, so a panic while holding
   it poisons the mutex and turns every subsequent request into a second panic.

2. **`read_head` reads a request line with no length bound.** The `MAX_HEAD` cap
   is checked only *after* a whole line is already in memory, so a single
   connection sending bytes with no newline grows the buffer until the process is
   OOM-killed. Loopback-only, but the crate already ships a CSP and an
   `addressed_to_us` origin check, so hostile local input is in the threat model.

## Current state

`web/src/http.rs`, the handler loop inside `serve` (around lines 287-291):

```rust
// The handler's whole life, on one thread. Ends when every connection
// thread and the accept loop have dropped their sender ...
for job in jobs {
    let response = handler(&job.request);   // <-- no catch_unwind; panics kill main
    let _ = job.reply.send(response);
}
Ok(())
```

`web/src/lib.rs:106,113` — the poison-prone locks:

```rust
let mut doc = doc.lock().expect("no request panics while holding the doc");
```

`web/src/http.rs`, `read_head` (around lines 377-395):

```rust
fn read_head(reader: &mut BufReader<TcpStream>) -> std::io::Result<Head> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let n = match reader.read_line(&mut line) {   // <-- unbounded read
            Ok(n) => n,
            Err(_) => return Ok(Head::Closed),
        };
        if n == 0 { /* ... */ }
        // ... later: if head.len() + n > MAX_HEAD { return Ok(Head::TooLarge) }
    }
}
```

`Head` is an enum with `Got(String)`, `Closed`, `TooLarge` — `TooLarge` already
exists and its caller already closes the connection (`http.rs:314`). `MAX_HEAD`
is a module constant near the top of `http.rs`.

Conventions to match:
- `web` has **zero external dependencies** — standard library only.
- Tests are in `#[cfg(test)] mod tests` blocks in `http.rs` and `lib.rs`; the
  `http.rs` block already builds `Request`s and calls `head_of`/`decode`
  directly, and `lib.rs`'s block builds a `Doc` and calls `route`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Test web | `cargo test -q -p gitten-web` | `test result: ok` |
| Lint | `cargo clippy -p gitten-web --all-targets` | no warnings |
| Format check | `cargo fmt --check` | clean |

## Scope

**In scope**:
- `web/src/http.rs` — wrap the handler call in `catch_unwind`; bound `read_head`.
- `web/src/lib.rs` — replace the two `.expect(...)` locks with poison-tolerant
  locking.

**Out of scope**:
- The routing logic in `route` — do not change what any endpoint returns.
- The connection-thread panic isolation — it is already correct.
- `Response`/`Request` shapes.

## Git workflow

- Branch: `advisor/005-web-handler-panic-isolation`
- Commits: one for the handler/lock isolation, one for the read bound, is clean.

## Steps

### Step 1: Catch panics around the handler call

In `serve`, wrap the handler call so a panic becomes a 500 instead of unwinding
out of `main`:

```rust
use std::panic::{catch_unwind, AssertUnwindSafe};

for job in jobs {
    let response = catch_unwind(AssertUnwindSafe(|| handler(&job.request)))
        .unwrap_or_else(|_| Response::status(500, "internal error"));
    let _ = job.reply.send(response);
}
```

Confirm `Response::status(u16, &str)` is the correct constructor (it is used
elsewhere in `http.rs`, e.g. the 431/405 replies). Match its exact signature.

**Verify**: `cargo build -p gitten-web` → exit 0.

### Step 2: Make the doc lock poison-tolerant

In `web/src/lib.rs`, replace both:

```rust
let mut doc = doc.lock().expect("no request panics while holding the doc");
```

with:

```rust
let mut doc = doc.lock().unwrap_or_else(|p| p.into_inner());
```

A poisoned mutex then still serves (the data behind it is a `Doc` that reflow
mutates; a half-applied reflow is recomputed on the next request, so recovering
the inner value is safe here).

**Verify**: `cargo build -p gitten-web` → exit 0.

### Step 3: Bound the head-line read

In `read_head`, cap each line read at `MAX_HEAD` so a newline-less flood cannot
grow unbounded. Replace the `reader.read_line(&mut line)` with a bounded read:

```rust
use std::io::Read;

let n = match (&mut *reader).take(MAX_HEAD as u64).read_line(&mut line) {
    Ok(n) => n,
    Err(_) => return Ok(Head::Closed),
};
```

Note `Read::take` consumes the reader, so use a reborrow (`(&mut *reader)`) or
restructure so the `BufReader` is not moved. If `take` on a `&mut BufReader`
fights the borrow checker, the alternative is to read bytes manually into a
`Vec<u8>` capped at `MAX_HEAD`, returning `Head::TooLarge` the moment the cap is
crossed, then `String::from_utf8_lossy`. Either is acceptable; the invariant is
that no single line can exceed `MAX_HEAD` bytes in memory.

After the read, the existing `head.len() + n > MAX_HEAD` check still returns
`Head::TooLarge`; keep it — Step 3 bounds the *line*, that check bounds the
*accumulated head*.

**Verify**: `cargo build -p gitten-web` → exit 0.

### Step 4: Tests

Add to `web/src/lib.rs` tests: a `route` call on `/api/rows` still works after a
previous request panicked. Simulating a real panic through `route` is awkward
without a panicking `Wrap`; instead, add a focused test in `http.rs` if a seam
allows, or assert the poison-tolerant lock directly: construct a `Mutex<Doc>`,
poison it (spawn a thread that locks and panics, join it), then confirm
`doc.lock().unwrap_or_else(|p| p.into_inner())` still yields a usable `Doc`.

For Step 3, add an `http.rs` test that a head line longer than `MAX_HEAD` yields
`Head::TooLarge` (feed a `BufReader` over a `&[u8]` of `MAX_HEAD + 100` non-`\n`
bytes). Follow the existing `http.rs` test style (they build readers over byte
slices — grep the test module for `BufReader` / `read_head`; if none exists,
model on how `connection` builds its `BufReader`).

**Verify**: `cargo test -q -p gitten-web` → all pass, including new tests.

## Test plan

- `web/src/http.rs`: an over-long head line returns `Head::TooLarge` (memory is
  bounded).
- `web/src/lib.rs`: a poisoned doc mutex is still served (recovery path).
- Verification: `cargo test -q -p gitten-web` → all pass.

## Done criteria

ALL must hold:

- [ ] `cargo test -q -p gitten-web` exits 0; new tests present and passing
- [ ] `grep -n "catch_unwind" web/src/http.rs` shows the handler wrapped
- [ ] `grep -n 'expect("no request panics' web/src/lib.rs` returns nothing
- [ ] `read_head` cannot grow a single line past `MAX_HEAD` (test proves it)
- [ ] `cargo clippy -p gitten-web --all-targets` clean; `cargo fmt --check` clean
- [ ] No files outside `web/src/http.rs` and `web/src/lib.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `catch_unwind` requires the closure to be `UnwindSafe` and `AssertUnwindSafe`
  does not resolve it (e.g. `handler` captures something genuinely unsafe to
  unwind across) — report the type error rather than adding `unsafe`.
- `Read::take` cannot be applied to the `BufReader` without a larger refactor and
  the manual-bytes alternative also fights the existing structure — report; do
  not rewrite `connection`.
- Removing the `.expect` breaks an existing test that relied on the panic — report.

## Maintenance notes

- If write verbs are ever added (roadmap), the handler-thread isolation becomes
  more important — a mutating handler that panics mid-write must not poison state
  that a retry reads. Revisit the recovery path then.
- A reviewer should confirm the 500 response body carries no internal error
  detail (the panic message must not reach the client).
