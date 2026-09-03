# Agent MCP adapter

An MCP server that lets an agent see a repository the way gitten does — rows,
meta, keys, dispatch, diffcheck, bench — without reimplementing any of it.
Stdio JSON-RPC, Python 3 stdlib only, no install step.

## Running it

```sh
# As an MCP server (what a client spawns):
python3 tools/mcp/server.py

# One tool, for scripts and debugging:
python3 tools/mcp/server.py call inspect.meta '{"view":"diff","repo":"."}'
python3 tools/mcp/server.py list        # the catalog as JSON

# The headless check:
tools/mcp/smoke.sh
```

A client config spawns the first form. With Claude Code's MCP support, that is
a stdio entry pointing at `python3` with `tools/mcp/server.py` as its argument
and the checkout as its working directory (`GITTEN_ROOT` overrides the root
when the adapter is copied elsewhere). The catalog it is offered is
`tools/mcp/tools.json` — six tools, served verbatim by `tools/list`.

## Tool catalog

| tool | what it answers |
|---|---|
| `inspect.rows` | A window of rows from `diff` or `commits`: `view`, `repo`, `rev` (revspec, or limit for commits), `from`, `count`, `cols`, `rows`, `layout`, `wrap`, `theme`, `via`. |
| `inspect.meta` | Everything a scroll does not change: label, counts, wrap state, theme, timings. |
| `inspect.keys` | The keymap. `[keys]` overrides from `gitten.toml` until a machine endpoint lands. |
| `dispatch` | A keypress → command name, the way every client resolves it. |
| `diffcheck` | Core's differs against git's own answer on a real repo (`repo`, `revspec`). |
| `bench` | `suite=quick` times one headless frame; `suite=full` runs core's pipeline bench (needs `fixtures/`). |

## Backends, in the order they are tried

1. **`inspect --json`** — the machine door, when WS1 lands. Feature-detected
   per process: probed once, never assumed. While it is absent everything
   below answers instead, and the reply says so.
2. **The web loopback API** — `/api/meta`, `/api/rows`, `/api/commits`. The
   adapter spawns `gitten-web` on a free loopback port, polls until it
   answers, GETs one route and reaps the server. `via: "web"` forces it;
   `via: "dump"` skips it.
3. **One headless terminal frame** — `cargo run -q --locked -p gitten-tui
   --example dump`, ANSI stripped, returned as `[{"text": ...}]` with the
   status line and the stderr timings beside it.

Every reply carries `backend` (which door answered) and, on a fallback,
`degraded: true` with a `note` saying what the caller is not getting. A tool
that cannot run — no fixtures for `bench full`, no server for `via: "web"` —
returns `ok: false` with the reason as JSON, never a traceback. That is the
graceful contract while WS1–WS4 are unmerged: valid JSON first, best door
available second.

## Why it is standalone

`tools/mcp/` is not a workspace member and holds no Rust. Everything arrives
through subprocesses — the same `dump` example `./dev dump` prints, the same
loopback routes a browser reads. That is deliberate: it proves rule 1 from the
outside. Anything a built-in does, an extension must be able to do too — and
here is the extension, written without touching `core/`, `shell/`, `web/src`,
`tui/`, or the workspace manifest.
