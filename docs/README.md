# gitten docs

`AGENTS.md` holds the philosophy — three rules, the boundary, the *don't*s. It is
short on purpose and stays short. This directory holds everything that would
otherwise bloat it: how each system actually works, what was decided and why,
which numbers those decisions rest on, and the diagrams.

## Systems

| | |
|---|---|
| [architecture.md](architecture.md) | crates, the boundary, what flows where |
| [clients.md](clients.md) | writing one: what is shared, what is yours, keys and commands |
| [diff-pipeline.md](diff-pipeline.md) | raw diff → rows on screen, stage by stage |
| [diff-rules.md](diff-rules.md) | differs, layouts and wrapping: what is wrong in the plausible direction |
| [gpui-notes.md](gpui-notes.md) | what GPUI does that you would not guess — read before touching `gui/` |
| [features.md](features.md) | every command, its keys, and where each client answers it (generated) |
| [syntax-highlighting.md](syntax-highlighting.md) | the scanner, the tables, the routing |
| [theming.md](theming.md) | colour as data, surfaces, contrast resolution |
| [commit-graph.md](commit-graph.md) | lane assignment, hues, the cap, the drawing |
| [terminal.md](terminal.md) | the terminal frontend, and what writing it moved into `core` |
| [worktrees.md](worktrees.md) | where worktrees live, and the checklist for removing one |
| [blobs.md](blobs.md) | bytes with no lines: classify, read, rasterize, draw |
| [extending.md](extending.md) | every seam, with a worked example each |
| [roadmap.md](roadmap.md) | build order from viewer to product, one block at a time |
| [competition.md](competition.md) | what else is out there, what to steal and when |

## Agents

The doors an agent works through, each with the command it runs. Start with
the first: an agent that cannot see the window cannot check its own work on it.

| | |
|---|---|
| [agent-verify.md](agent-verify.md) | the window, driven and photographed headless — no focus taken |
| [agent-inspect.md](agent-inspect.md) | `gitten inspect` / `dispatch`: rows, meta and keys as data |
| [agent-json.md](agent-json.md) | the `--json` contract every headless tool shares |
| [agent-mcp.md](agent-mcp.md) | the same tools over MCP |
| [agent-web.md](agent-web.md) | the loopback HTTP API |
| [agent-perf.md](agent-perf.md) | the perf gate: is it slower, without fooling yourself |

`tools/lint.sh` (`./dev lint`) holds the `AGENTS.md` rules a grep can check,
and both the pre-commit hook and `./check.sh` run it. `./dev features`
regenerates [features.md](features.md), and `./check.sh` fails when it is stale.

## Decisions

[decisions/](decisions/) — one file per decision, newest last. Read these before
changing anything they cover; each records what was tried, what the numbers said
and what it would take to revisit.

## Evidence

[measurements.md](measurements.md) — every number quoted anywhere else, with the
command that produced it. A figure in a decision record that is not reproducible
here is a figure to distrust.

## Keeping this honest

Three habits, and they matter more than completeness:

**Write what is, not what is planned.** A doc describing an unbuilt feature is
worse than no doc, because it costs a reader real time before they find out. Say
"not built" and move on — [architecture.md](architecture.md) has a section for it.

**Numbers carry their command.** Every figure names how to reproduce it. Hardware
changes, fixtures change, and an unreproducible number quietly becomes folklore.

**Detail belongs next to the code first.** If a rule only matters while reading
one function, it is a doc comment, not a page here. These pages are for what
spans files: a pipeline, a boundary, a decision, a measurement.
