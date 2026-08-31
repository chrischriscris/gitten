# Plan 025: A startup failure opens the window and says so, instead of dying on stderr

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **NEVER launch the desktop client during this work** (repo rule: a window
> appearing unannounced interrupts whoever is at the keyboard). Verify with
> the headless tests below and hand the operator the manual smoke commands at
> the end.
>
> **Drift check (run first)**: `git diff --stat 87229df..HEAD -- app/src/lib.rs app/src/acquire.rs shell/src/main.rs shell/src/views/commits.rs shell/src/views/diff.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug / UX
- **Planned at**: commit `87229df`, 2026-08-30

## Why this matters

Every startup failure in the desktop app — git binary missing, not a
repository, a repo with no commits, even a **clean working tree** ("no
changes" is an `Err` today) — prints one line to stderr and exits before any
window exists. A user double-clicking `target/gitten.app` (built by
`./dev bundle`, launched from Finder, no stderr anywhere) sees the dock icon
bounce and vanish with no explanation. Meanwhile the shell already has an
error band UI (`BAND_H`, `DevShell.error`) — it is just wired only to
*re-diff* failures that happen after the window is up. This plan makes the
window always open, with acquisition failures shown in that band over an
empty view. (Moving acquisition off the first frame entirely — async with a
loading state — is deliberately deferred; see Maintenance notes.)

## Current state

- `app/src/lib.rs:248-290` — `Startup::go()` does three things in sequence
  and collapses them into one `Result`: parse args (→ `Exit::Help` on
  `-h`), load `gitten.toml`, then acquire, where any failure becomes
  `Exit::Failed`:

  ```rust
  pub fn go(self) -> Result<Started, Exit> {
      let mut clock = StartClock::new();
      let request = cli::parse(&self.args, self.default);
      // ... Help / Config early returns, config::load into host ...
      match acquire::acquire(view, &source, &host) {
          Ok(loaded) => Ok(Started { view, source, host, loaded, config: path }),
          Err(e) => Err(Exit::Failed(format!("{}: {e}\n\n{}", self.binary, self.usage()))),
      }
  }
  ```

- `app/src/lib.rs:142-154` — `Exit::finish()` prints `Failed` to stderr and
  `process::exit(1)`.

- `app/src/acquire.rs` — the failures in question. A clean tree and an empty
  repo are errors:

  ```rust
  // app/src/acquire.rs:108-113 (diff view)
  if files.is_empty() {
      let what = match arg.is_empty() { true => "(working tree)", false => arg.as_str() };
      return Err(format!("no changes for {} {what}", path.display()));
  }
  // app/src/acquire.rs:156-158 (commits view)
  if commits.is_empty() {
      return Err(format!("no commits in {}", path.display()));
  }
  ```

  Real git failures surface through `gitten_git`'s `run` as
  `could not run git: {e}` / `git {args}: {stderr}` strings.

- `shell/src/main.rs:567-580` — the shell exits before GPUI starts:

  ```rust
  let started = match Startup::new("gitten", View::Commits) /* ... */ .go() {
      Ok(started) => started,
      Err(exit) => exit.finish(),
  };
  ```

  Everything after destructures `Started` (`view`, `source`, `host`,
  `loaded`, `config`) and builds `rediff`, the title, and — inside
  `app.run` — the view entities at `main.rs:731-766` by matching on
  `Data::Commits(commits)` / `Data::Diff(files)`.

- `shell/src/main.rs:157, 185-194, 216` — `DevShell` has
  `error: Option<SharedString>`; set today only by the re-diff path
  (`Err(e) => self.error = Some(e.into())`) and cleared on success. Rendered
  at `main.rs:448, 521-534` as a one-sentence band under the title bar.

- `shell/src/views/commits.rs:76-110` — `Commits::new(commits, host)`
  computes `widest` with `.max_by(...).map(|(i, _)| i).unwrap_or(0)`, so an
  empty commit list yields `widest = 0` — then `render` passes
  `.with_width_from_item(Some(self.data.widest))` to a `uniform_list` of 0
  items. Whether GPUI tolerates "measure item 0 of 0" is unverified — Step 3
  settles it with a test.

- Other clients (`tui`, `web`) call `Startup::go()` too and must keep exactly
  today's behaviour — a terminal client dying to stderr is correct.

- Conventions: `shell/` has headless GPUI tests (`#[gpui::test]`) that run in
  CI (`cargo test -p gitten-shell`); `app` has plain unit tests. Comments
  explain why, not what.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| App tests | `cargo test -q -p gitten-app` | exit 0 |
| Shell tests (headless) | `cargo test -q -p gitten-shell` | exit 0 |
| Other clients unaffected | `cargo test -q -p gitten-tui -p gitten-web` | exit 0 |
| Whole workspace builds | `cargo build -q --workspace` | exit 0 |
| Lint / format | `cargo clippy -q --workspace --all-targets -- -D warnings && cargo fmt --check` | exit 0 |
| Full gate | `./check.sh` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `app/src/lib.rs` (split `go()`; add `Ready`)
- `shell/src/main.rs`
- `shell/src/views/commits.rs` (only if the empty-list test in Step 3 fails)
- `plans/README.md` (status row)

**Out of scope** (do NOT touch, even though they look related):
- `app/src/acquire.rs` — "no changes is an Err" stays; the *shell* reinterprets
  it. Changing acquire's contract changes tui and web behaviour.
- `tui/`, `web/` — they keep `go()` and today's exit behaviour.
- Async/background acquisition, loading spinners — explicitly deferred.
- `session.rs` — the resume path only runs when data exists; don't rework it.

## Git workflow

- Branch: `advisor/015-startup-failures-open-a-window`
- Commit style: imperative sentence, why-first, no prefix — e.g.
  `Open the window on an acquisition failure and say what failed`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: split acquisition out of `Startup::go()` in `app`

In `app/src/lib.rs`, refactor `go()` into two stages without changing its
public behaviour:

```rust
/// Everything before acquisition: arguments parsed, config loaded. What a
/// client that wants to put acquisition failures on its own screen calls;
/// `go()` remains the one-call door for clients that exit on failure.
pub struct Ready {
    pub view: View,
    pub source: Source,
    pub host: Host,
    pub config: PathBuf,
    binary: &'static str,
    usage: String,
}

impl Startup {
    pub fn prepare(self) -> Result<Ready, Exit> { /* args + config, Help/Config early-outs */ }
    pub fn go(self) -> Result<Started, Exit> {
        let ready = self.prepare()?;
        ready.acquire().map_err(|e| /* same Exit::Failed formatting as today */)
    }
}

impl Ready {
    /// The acquisition half. The error is the bare message — the caller
    /// decides whether it becomes an exit code or a sentence in a band.
    pub fn acquire(self) -> Result<Started, (String, Self)> { /* or acquire(&self) -> Result<Loaded, String>; pick the shape that keeps go() simple */ }
}
```

Keep field visibility minimal-but-sufficient (the shell needs `view`,
`source`, `host`, `config`). Preserve the `StartClock` stages. The exact
signature of `acquire` is yours; the hard requirement is: **`go()`'s
observable behaviour is byte-identical** (same error string format including
the usage suffix, same exit paths), because tui and web ship on it.

**Verify**: `cargo test -q -p gitten-app` → exit 0.
`cargo test -q -p gitten-tui -p gitten-web` → exit 0.

### Step 2: the shell opens the window either way

In `shell/src/main.rs`, replace the `go()` call with `prepare()` +
`acquire()`:

- `prepare()` errors (`Exit::Help`, `Exit::Config`, and arg-parse failures)
  keep calling `exit.finish()` — those come from a terminal invocation and
  belong on stdio.
- An `acquire()` error keeps the `Ready` parts and enters `app.run` with
  empty data for the requested view (`Data::Commits(vec![])` /
  `Data::Diff(vec![])` — check `Data`'s definition in `gitten_app` and
  construct accordingly) and the error message carried into `DevShell`'s
  existing `error` field at construction (`main.rs:829` currently hardcodes
  `error: None` — thread the startup message in there).
- The title falls back to the source's label-less form (there is no `loaded.label`
  on failure — use the repo path or the binary name; look at
  `started_title(which, &loaded.label)` at `main.rs:~610` and give it a
  reasonable input).
- Skip `session::restore` when data is empty (resume into nothing is a no-op
  anyway — verify it doesn't panic; if it does, guard it).
- `rediff` stays `None` on the failure path.

Everything else (window options, menus, quit wiring) runs unchanged, so a
failed startup produces: title bar + band with the message + empty view.

**Verify**: `cargo build -q -p gitten-shell` → exit 0.
`cargo test -q -p gitten-shell` → existing tests still pass.

### Step 3: prove the empty window is safe, headlessly

Add a `#[gpui::test]` to the shell (follow the structure of the existing
GPUI tests — find them with `grep -rn 'gpui::test' shell/src`) that builds
`Commits::new(vec![], host)` and `Diff::new(vec![], host, cx)` and lays each
out in the headless window, asserting no panic and zero rows rendered. If
`uniform_list(..., 0 items).with_width_from_item(Some(0))` panics or asserts
in layout, guard the call site:
`.with_width_from_item((!commits.is_empty()).then_some(self.data.widest))`
— same for the diff view's equivalent if needed.

**Verify**: `cargo test -q -p gitten-shell` → exit 0 including the new test(s).

### Step 4: full gate + handover

**Verify**: `./check.sh; echo $?` → 0;
`cargo fmt --check && cargo clippy -q --workspace --all-targets -- -D warnings` → exit 0.

In your final report, hand the operator these manual smoke checks (do NOT run
them yourself — they open windows):

```sh
cargo run -q -p gitten-shell -- /tmp/definitely-not-a-repo   # window + band: "git ...: not a repository..."
cd "$(mktemp -d)" && git init -q && cargo run -q -p gitten-shell -- diff .   # window + band: "no changes for ..."
```

## Test plan

- New: headless empty-view layout test(s) in `shell` (Step 3).
- New: `app` unit test asserting `prepare()` + `acquire()` composes to the
  same `Started`/error as `go()` on a fixture (the repo itself is the fixture
  the existing acquisition tests use — model on whatever
  `cargo test -p gitten-app` already does for `go()`; if no such test exists,
  add one for the error-string format so the split can't drift).
- Existing suites all green.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -n 'fn prepare' app/src/lib.rs` and `grep -n 'fn acquire' app/src/lib.rs` hit
- [ ] `grep -n 'exit.finish()' shell/src/main.rs` still exists (Help/Config path) but the acquire-failure path no longer reaches it (read the diff)
- [ ] `cargo test -q -p gitten-app -p gitten-shell -p gitten-tui -p gitten-web -p gitten-core -p gitten-git` exits 0
- [ ] New empty-view test exists and passes
- [ ] `./check.sh` exits 0; fmt and clippy clean
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- The `Started` destructuring in `shell/src/main.rs` turns out to feed data
  into more than the sites named here (title, session key, rediff, views) and
  the empty path would need to fabricate values with meaning — list the sites
  and stop.
- GPUI's `uniform_list` cannot be made safe on zero items with the one-line
  guard in Step 3.
- Making `go()` byte-identical requires changing `Exit` or `Started`'s public
  fields in a way that breaks tui/web compilation.
- The code at the "Current state" locations doesn't match the excerpts.

## Maintenance notes

- **Deferred, deliberately**: moving acquisition off the first frame (open
  instantly with a "reading `<repo>`…" state, acquire on
  `cx.background_executor()`, swap data in). This plan creates the seam for
  it — `Ready::acquire()` is exactly the call a background task would make —
  but doing it needs view-swap plumbing and a loading design that deserve
  their own pass. On a warm local repo acquisition is ~150 ms
  (`docs/measurements.md`), so the synchronous window is acceptable until a
  slow-repo complaint is real.
- A future "watch mode / job runner" (roadmap Phase A#3) should reuse the
  same band + empty-view shapes for transient failures.
- Reviewers: confirm the tui and web binaries behave byte-identically
  (`Exit::Failed` text unchanged — the usage suffix matters, tests pin it),
  and that the clean-tree case reads as calm ("no changes…" in a quiet band
  over an empty diff), not alarming.
