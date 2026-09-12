# 003 — W10a: external tools, custom commands and the command log

Packet **W10**, first half, from
[001 — TUI lazygit parity swarm](001-tui-lazygit-parity-swarm.md). One worker, three
slices. Read 001 §"Architectural requirements", §"Swarm execution contract" and
§"Verification gates" first; they bind this document.

Its dependencies (W2 input, W4 sync) are landed. It does not depend on W7 and does
not touch it. [004](004-w10b-view-conveniences-and-copy.md) is the other half of
W10 and is deliberately disjoint from this one: nothing in 004 spawns a process,
nothing here changes a view's shape.

## Objective

Everything gitten cannot do today because it will not run another program: the
editor, the file opener, the difftool, `git commit` with the user's own editor,
editing a hunk by hand, a shell command from a prompt, and the user's own
configured commands on their own keys. One execution service, one terminal
lifecycle, one argv rule — and after it, a command log so the user can see what
was actually run.

The whole packet is one seam plus its tenants. If you find yourself writing a
second way to spawn something, stop: the first one is wrong.

## Ledger rows

| ID | Row | Importance | Slice |
|---|---|---|---|
| LG-095 | Edit file in external editor (`e`) | daily | 1 |
| LG-096 | Open file in default application (`o`) | minor | 1 |
| LG-094 | Open external diff tool (`ctrl+t`) | minor | 1 |
| LG-112 | Suspend the application (`ctrl+z`) | minor | 1 |
| LG-098 | Commit with git editor (`C`); commit without pre-commit hook (`w`) | daily | 2 |
| LG-097 | Edit hunk in external editor (`E`) | advanced | 2 |
| LG-099 | Execute shell command from prompt (`:`) | advanced | 2 |
| LG-100 | Edit config file in external editor | minor | 2 |
| LG-114 | Custom command keybindings with context parameters | advanced | 3 |
| LG-101 | Command log options (`@`) | minor | 3 |
| LG-092 | PR: create, open in browser, copy URL | minor | 3 |
| LG-093 | Open commit in browser (`o`) | minor | 3 |

Do not edit `tui-parity-ledger.md`; the coordinator owns it.

## Base, worktree, ownership

```sh
# from /Users/chus/Projects/gitten, once:
git worktree add .worktrees/tui-parity-w10a -b feat/tui-parity-w10a 529ec80
```

- **Your only working directory** is `.worktrees/tui-parity-w10a`. Never write to
  `/Users/chus/Projects/gitten`, `/Users/chus/Projects/gitten-ux-polish`, or another
  `.worktrees/*`. Never push, never write to a remote, never open a PR. Commit
  locally, one commit per working increment.
- **Owned outright:** `core/src/tool.rs` (new), `core/src/forge.rs` (new),
  `app/src/tools.rs` (new), `tui/src/term.rs`, and your tests.
- **Shared, additive only**, each edit inside a `// --- w10a begin` / `// --- w10a
  end` block: `core/src/lib.rs`, `core/src/command.rs`, `app/src/config.rs`,
  `app/src/lib.rs`, `git/src/lib.rs` (the recorder hook only), `tui/src/main.rs`,
  `shell/src/main.rs` (one deletion — see slice 1).
- Do not touch `tui/src/stashes.rs` (W7), `tui/src/panes.rs` or `tui/src/files.rs`
  (004), or any `shell/src/*` beyond the single call site named below.

## What already exists — do not rebuild it

- **`OsStrExt::from_bytes` is the established way a raw path becomes an argv item**
  — `git/src/lib.rs`'s `run_bytes`, `run_stdin` and `run_env` all do it. Your spawn
  path does the same. There is no `cfg(target_os)` anywhere in this tree and yours
  must not add one.
- `run_stdin` exists because *a payload is not argv*. Same instinct here: a commit
  message, a patch, a diff never rides a command line.
- `write_todo_tmpfile` (in `git/src/lib.rs`) already solves "hand a file to another
  program safely": `create_new`, an unguessable name, mode `0600`, one file per
  call. Slice 2's hunk editor needs exactly those four properties — read its doc
  comment and reuse the reasoning rather than reinventing a temp file.
- `rebase_todo` already runs git with `GIT_SEQUENCE_EDITOR` pointed at a script, and
  sets `GIT_EDITOR=true` so the *second* editor cannot block. Slice 2's "commit with
  the git editor" is the same mechanism with the value the user configured instead
  of `true`, and `core/src/rebase.rs`'s doc comments already name every place git
  opens an editor.
- `tui/src/term.rs` owns raw mode and the alternate screen: `Terminal::enter(mouse)`,
  `leave()`, a panic `guard()`, `copy()` over OSC 52, `size()`, `poll()`. Every
  terminal handoff goes through this file and nowhere else; the rest of the terminal
  client is testable with no terminal at all, and that stays true.
- `shell/src/main.rs` **already opens `gitten.toml` in `$EDITOR`, detached**. That
  is the whole bug this packet fixes in miniature: the window can launch an editor
  and no other client can, exactly as the config parser once lived behind GPUI.
  Slice 1 moves it behind the shared service and leaves one call at that site.
- `app/src/config.rs` parses the tables `font`, `theme`, `diff`, `view`, `mouse`,
  `keys`. `[tools]` and `[[commands]]` are new tables in the same file, and
  `./dev config` must emit them.
- `Commands::register(name, doc)` and `Keymap::bind(mode, chord, command)` are both
  public. Config-defined commands are registered through them — that is what makes
  the help panel list them and `[keys]` remap them. A parallel keymap for custom
  commands is a rule-1 violation and will be rejected.
- `notify` already watches `gitten.toml` and reloads it on the next frame, so
  slice 2's config editor needs no reload of its own.

## The argv rule

This is the packet's one non-negotiable design constraint, and it comes from 001
§"Architectural requirements" 4 and W10.2:

- A tool is **argv**: a program plus a list of arguments, each argument either a
  literal or one whole placeholder. Placeholders substitute *as complete argv
  items* — never into the middle of a word, never into a shell string.
- A user who wants a shell writes `shell = "…"` explicitly. That snippet is passed
  to `sh -c` **verbatim, with nothing interpolated into it**. Context reaches it
  through the environment: `GITTEN_FILE`, `GITTEN_FILE_ABS`, `GITTEN_SHA`,
  `GITTEN_BRANCH`, `GITTEN_REMOTE`, `GITTEN_REF`, `GITTEN_LINE`, `GITTEN_REPO`.
  Environment values are bytes, set through `OsStr::from_bytes`.
- Therefore a path containing `;`, `$(…)`, a quote, a newline or a non-UTF-8 byte is
  data in both modes, and a filename cannot become code. Prove it with tests, in
  both modes.

`core::tool` holds this and nothing else: a `Template` (literals and placeholders),
a `Context` (the selection, as bytes), and `resolve(&Context) -> Vec<Vec<u8>>`. It is
pure, it has no dependencies, and it is where the substitution tests live.

## Slice 1 — the execution service and the terminal lifecycle

`app::tools` resolves *what to run* and *how*:

- Resolution order for the editor: `[tools] editor` in `gitten.toml`, then
  `$GIT_EDITOR`, `$VISUAL`, `$EDITOR`, then a refusal that says which variables were
  consulted. "No editor configured" is a sentence, not a silent no-op — W0's
  contract.
- The opener defaults to the platform's, and this is the one place a platform fact is
  honest: `open` on macOS, `xdg-open` where it exists. Resolve it by *looking for the
  binary*, not by `cfg(target_os)` — AGENTS.md's rule holds, and a lookup is
  portable for free. `[tools] open` overrides.
- The difftool and mergetool default to git's own configuration (`git difftool`,
  `git mergetool`) rather than a second opinion about which one the user has.
- A `Launch` describes: argv or shell snippet, cwd, env additions, and a **mode**:
  `Foreground` (the client must yield its display, wait, then restore and refresh)
  or `Background` (detached; output captured for the log, never for the screen).

`tui/src/term.rs` grows one method — `Terminal::handoff(&mut self, f)` — that
leaves the alternate screen and raw mode, runs `f` with stdio inherited, and
re-enters on **every** exit path: success, failure, signal, panic. Restoration on
error is the whole point; `guard()` covers a panic today and the new path must not
be the exception. After restoring: a full redraw and a `repo.refresh`, because the
file on disk may be different now.

`ctrl+z` (LG-112) is the same lifecycle with `SIGTSTP` in the middle: leave, raise,
and on resume re-enter and redraw. `libc` is already in `Cargo.lock` (0.2.189, via
other crates) so a `libc` line in `gitten-tui` costs no download; `signal-hook` is
already crossterm's own dependency and is the other honest option. Pick one, confine
it to `term.rs`, and say in the module doc why the second dependency exists — this
crate deliberately has two.

**Tenants of the service in slice 1:** `files.edit` / `diff.edit` (`e`),
`files.open` (`o` — and note the ledger's LG-092 collision: `o` is `project.switch`
today; do not repurpose a shipped key, put the row's opener elsewhere and record the
question for the coordinator), `files.difftool` / `commits.difftool` (`ctrl-t`), and
`view.suspend` (`ctrl-z`).

**Acceptance (slice 1).** A fake tool — a small script written into a temp dir that
appends its argv, NUL-separated, to a file — records exactly what was passed for: a
path with a space, a path with a single quote, a path with `$(touch pwned)` in it, a
path with a non-UTF-8 byte, and the same five in shell mode where the payload
arrives in the environment. Nothing named `pwned` exists afterwards. The lifecycle
is tested through a seam that records enter/leave order, including the failing and
the interrupted child; **no test launches a real editor, browser or difftool**, and
no test takes over the developer's terminal.

## Slice 2 — the four editor-shaped workflows

- **`C` — commit with the git editor.** `git commit` with `GIT_EDITOR` set to the
  resolved editor, foreground, through the write queue like every other write.
  `w` — commit with `--no-verify`, which is a different command name (`files.commit
  -no-verify`-shaped, spelled as its own name), never a flag smuggled into the first.
  Both refresh on finish, success or refusal.
- **`E` — edit a hunk.** Emit the hunk as a patch with `core::patch` (W3's emitter,
  already the source of truth for staging a hunk), write it to a temp file with
  `write_todo_tmpfile`'s four properties, open the editor foreground, re-read the
  bytes, and apply through the existing `stage_patch` path. If the edited patch does
  not apply, **change nothing** and say why — a partially applied hand-edited patch
  is the worst outcome available here.
- **`:` — a shell command from the prompt.** The user's typed line goes to `sh -c`
  verbatim. Nothing is interpolated into it, no selected path is appended, and it is
  distinct from a configured custom command (001 W10.2 says so explicitly). It runs
  foreground, its output is kept and shown, and its exit status is reported.
- **Config in the editor** (LG-100): open `gitten.toml` — the resolved path, from
  the config loader, not a guess — foreground; the existing watcher reloads it. Then
  delete the detached-editor code in `shell/src/main.rs` and call the service
  instead. One line changes there; nothing else in `shell/` is yours.

**Acceptance (slice 2).** With the fake editor: a commit whose message the editor
writes lands with that message; `--no-verify` skips a planted failing pre-commit
hook while the plain commit is refused by it; an edited hunk that adds a line stages
exactly that line, and a corrupted one stages nothing and reports; `:` with
`printf '%s' "$SOMETHING"` returns its output and a non-zero exit is surfaced;
cancelling any prompt spawns nothing (assert the recorder file does not exist).

## Slice 3 — custom commands, the log, and forge URLs

**Custom commands (LG-114).** In `gitten.toml`:

```toml
[[commands]]
key = "ctrl-r"           # bound through Keymap::bind, remappable in [keys]
mode = "files"           # an existing keymap mode; an unknown mode is an error
name = "custom.rerun"    # the command name help lists
doc = "run the test suite on the selected file"
argv = ["cargo", "test", "--", "{{file}}"]   # placeholders are whole argv items
# or: shell = "cargo test -- $GITTEN_FILE"   # verbatim; context via environment
prompts = [{ kind = "input", title = "extra args" }]
output = "log"           # none | log | panel
mode_of_run = "background"
```

Registered at config load through `Commands::register` and `Keymap::bind`, so they
appear in `?`, resolve through the same dispatch as a built-in, and can be rebound in
`[keys]`. A malformed entry is a named config error, not a silent drop. A cancelled
prompt runs nothing.

**The command log (LG-101).** The user cannot currently see what gitten ran. Add a
recorder to `gitten-git`: an optional `Arc<dyn Fn(&Invocation)>` on the `Binary`,
called from `run`, `run_bytes`, `run_stdin` and `run_env` — the four call sites, no
fifth path. `Invocation` is a `core` type (program, argv as bytes, duration, exit
status, first line of stderr on failure). Off unless a client sets it. Two rules:
**a credential never reaches the log** (a URL with userinfo is redacted at
construction, not at display), and the log is bounded (a ring, oldest dropped) so a
long session cannot grow without limit. `@` opens it; the same model serves the
window later for free.

**Forge URLs (LG-092, LG-093).** `core::forge` turns a remote URL into a web URL:
`https://`, `git@host:owner/repo.git`, `ssh://git@host/owner/repo`, a port, a
trailing `.git`, a self-hosted path prefix. GitHub, GitLab and Bitbucket shapes for
commit, branch-compare and pull-request-create. Pure, no network, table-driven
tests — this is the module where a wrong answer is cheap to prove.

Opening is the slice-1 opener. **Creating a pull request is opening the forge's
compare URL in a browser** — no API call, no token, no publishing, which is both
what lazygit does by default and the only version of this row that is safe for an
agent to write. `ctrl+y` copies the URL through the existing clipboard path.

**Acceptance (slice 3).** A config with three custom commands: all three appear in
help with their keys, one runs with a path containing a space and a `;` (argv
recorder proves both arrived as single items), one in shell mode reads the same path
from the environment, one is cancelled at its prompt and spawns nothing. An unknown
mode and a missing `argv`/`shell` each produce a named config error. The recorder
logs a read and a write, redacts `https://user:token@host/…`, and drops the oldest
entry at the ring's bound. `core::forge` has a table test per URL shape including one
that must **not** produce a URL.

## Hazards

- **The user is at this keyboard.** Never launch `./dev tui`, `./dev desktop` or a
  real editor/browser. `./dev dump` is the frame you look at; hand over any command
  that needs a human.
- A foreground handoff that fails to restore the terminal makes the app look like it
  crashed. Test the failure paths, not just the happy one.
- The write queue serializes writes; a foreground tool that blocks the queue while
  waiting on a human is a hang. Decide explicitly where a tool waits, and say so in
  the module doc.
- Do not repurpose a shipped key to match upstream. `o`, `y`, `s` and `D` already
  mean something here; a key change is a product decision, recorded for the
  coordinator, never a quiet edit.
- `[tools]` values are paths and may contain spaces; a configured editor is argv,
  not a string to split on whitespace unless the user wrote a shell snippet.
- Nothing in this packet may add a dependency to `core/`.

## Gates

```sh
cargo fmt --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p gitten-core -p gitten-app -p gitten-git -p gitten-tui
cargo test --locked -p gitten-core -p gitten-app -p gitten-git -p gitten-tui tui_parity_
COLS=120 ROWS=40 ./dev dump commits .
./dev config | head -80      # the new tables must appear, and be correct
```

`./dev check` is the coordinator's to schedule — it rewrites generated fixtures and
must not run beside another lane.

## Proof protocol

Two lanes in this campaign fabricated completion reports, so:

1. End the report with verbatim `git log --oneline 529ec80..HEAD` and
   `git diff --stat 529ec80..HEAD`.
2. Every claimed commit resolves under `git cat-file -e`.
3. Every claimed test is named from captured test-binary output.
4. Gate tails are pasted with exit statuses, not summarized.
5. A failing gate is reported as failing. A smaller true report beats a larger false
   one.

## Out of scope

The view conveniences in [004](004-w10b-view-conveniences-and-copy.md) — diff
options, file tree, screen modes, the copy family. W7's stash/tag/reflog. W8's patch
builder. The desktop window ([005](005-desktop-picks-up-the-campaign.md) owns it),
beyond deleting the one detached-editor call site named in slice 2. Any network call
to a forge API.

## Dispatch text

> You are the W10a implementation lane for gitten. Read
> `plans/003-w10a-external-tools-and-custom-commands.md` in full, then
> `plans/001-tui-lazygit-parity-swarm.md` and `AGENTS.md`. Work only in
> `.worktrees/tui-parity-w10a` on `feat/tui-parity-w10a`, based on 529ec80. Slice 1
> is the seam every later slice uses — land it, with its tests, before starting
> slice 2. Commit after each working increment. Never push, never touch another
> worktree, never launch a client or a real editor/browser: every test drives a
> recorder script. Finish with the plan's proof protocol.
