# The agent door: `gitten inspect` + `gitten dispatch`

A non-interactive client for agents and scripts. `inspect` reads state out as
human text or JSON; `dispatch` steps named commands through a headless viewport
the way the window and the terminal step them through a visible one.

```sh
cargo run -q -p gitten -- inspect keys --json
cargo run -q -p gitten -- dispatch --run down,down --json
```

Everything above drawing is the shared path: `gitten.toml` via `gitten-app`'s
config loader, acquisition via `gitten-app`'s `acquire` over a `gitten-git`
handle, and the same `gitten-core` projections the window and the terminal read
(`Flat::report`, `Document::report`, `Keymap::help`/`live_keys_for`,
`Themes::names`, `Wraps::names`, `Differs::selected`). Layouts are the one
deliberate exception: the registry is frontend-owned, so `inspect layouts`
reports the configured name plus the names every client agrees on, and says so.

## `inspect`

```text
gitten inspect rows [REPO] [REVSPEC] [--json]   the diff as counts and file lines
gitten inspect meta [REPO] [LIMIT] [--json]     the repository and the host behind it
gitten inspect keys [MODE] [--json]             what each key runs, now
gitten inspect themes|wraps|layouts [--json]    the registries and what is selected
gitten inspect config [--json]                  gitten.toml as it reads back
gitten inspect graph [REPO] [LIMIT] [--json]    commits with lanes, hues and merges
```

`--json` switches every topic to a machine envelope:

```json
{ "schema": "gitten.inspect/1", "kind": "rows", ... }
```

`kind` is the topic name (`dispatch` for the harness below). `keys` reads its
positional as a *mode* rather than a repository — it needs no data, only the
keymap — and defaults to every mode the keymap binds. `rows` also carries the
rendered-Markdown report (`markdown`, from `Document::report`) whenever a `.md`
file is in the diff, and empty otherwise. `graph` carries one plan row per
commit (`sha`, `lane`, `hue`, `merge`, `lanes`, `capped`); `lanes` is the honest
uncapped count from `core::graph::lane_count`, and human output shows the first
20 rows.

## `dispatch`

```text
gitten dispatch [VIEW] [REPO] [ARG] --run cmd,cmd,... [--height N] [--json]
```

`VIEW` is `diff` or `commits` and defaults to commits — history always loads,
while a working-tree diff is empty on a clean checkout. Each `--run` entry is a
command name (`view.down`) or a key spelling (`j`, `down`, `ctrl-d`) resolved
through the same `core::command` path every client uses: registry names first,
then the keymap against the view's mode stack. `--height` sizes the viewport
page commands measure against (default 24).

Each step reports `{step, input, command, cursor, top, selection, status}`.
`selection` is what the cursor sits on (`short subject` for a commit, the row
text for a diff row). `status` is one of:

- `ok` — the command ran.
- `noop` — acknowledged and intentionally state-free (`quit`, `view.left` on a
  commit list, pane focus in a single view).
- `wrong-view` — a real command aimed at the other view.
- `needs-client` — a real command only a live client can run (search prompts,
  `commits.open-diff`, refresh).
- `refused` — a write verb. Dispatch never stages, discards, resets, pushes or
  otherwise mutates the repository; anything under `files.`, `branches.`,
  `stashes.`, `rebase.`, the hunk verbs and the history rewrites is refused.
- `unknown-command` — neither a command name nor a key spelling.
- `pending` — the start of a longer chord (a custom `gitten.toml` can produce it;
  the shipped map never does).

## Examples

What can I do here, and what is selected?

```sh
gitten inspect keys diff --json | head -30
gitten dispatch --run down,down --json
```

Which files changed in the last two commits, and how big?

```sh
gitten inspect rows . HEAD~2..HEAD
```

Is this history wide, and where are the merges?

```sh
gitten inspect graph . 200 --json
```
